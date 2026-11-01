//! Sessions in the OS terminal for `aisbx-ui`. [`Launch`] runs a [`Handoff`]'s pre-start steps in
//! the background, then [`launch_argv`] and [`open`] start the session in a terminal window.
//! The session never becomes shell text, except as POSIX-quoted words in the macOS `do script`.

use std::cell::RefCell;
use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::app::Handoff;
use crate::job::Progress;

/// The Unix wrapper: runs the session from its positional arguments and, after a non-zero exit,
/// keeps the window open until Enter.
pub const UNIX_WRAPPER: &str = "\"$@\"; s=$?; if [ \"$s\" -ne 0 ]; then printf \
     \"\\naisbx: exited with %s - press Enter to close\\n\" \"$s\"; read -r _; fi; exit \"$s\"";

/// `$0` of the wrapper — what `ps` shows for it.
const WRAPPER_NAME: &str = "aisbx-session";

/// A terminal that exits non-zero this soon after its start did not open the window.
const QUICK_FAILURE: Duration = Duration::from_secs(3);

/// Linux terminal emulators, in the order they are looked for, with the arguments that make each
/// run the command after them.
pub const LINUX_TERMINALS: [(&str, &[&str]); 9] = [
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("wezterm", &["start", "--"]),
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("foot", &[]),
    ("xterm", &["-e"]),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    MacOs,
    Linux,
    Windows,
}

impl Os {
    /// The OS this binary was built for; any other Unix is treated as Linux.
    pub fn current() -> Os {
        if cfg!(target_os = "macos") {
            Os::MacOs
        } else if cfg!(windows) {
            Os::Windows
        } else {
            Os::Linux
        }
    }
}

/// What [`launch_argv`] reads of the environment.
pub struct TermEnv<'a> {
    /// `AISBX_TERMINAL`: the words that open a terminal running the command after them.
    pub aisbx_terminal: Option<String>,
    pub path: Option<OsString>,
    /// `PATHEXT`, read on Windows only (`.COM;.EXE;.BAT;.CMD` when unset).
    pub pathext: Option<OsString>,
    /// Whether a file exists — `Path::is_file` outside the tests.
    pub exists: &'a dyn Fn(&Path) -> bool,
    /// `$VISUAL`, else `$EDITOR`: the user's own editor, which runs in a terminal.
    pub editor: Option<String>,
}

impl TermEnv<'static> {
    /// This process's environment.
    pub fn from_process() -> Self {
        TermEnv {
            aisbx_terminal: std::env::var("AISBX_TERMINAL").ok(),
            path: std::env::var_os("PATH"),
            pathext: std::env::var_os("PATHEXT"),
            exists: &|path| path.is_file(),
            editor: ["VISUAL", "EDITOR"]
                .into_iter()
                .filter_map(|v| std::env::var(v).ok())
                .find(|e| !e.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchError {
    /// No terminal emulator was found and `AISBX_TERMINAL` is not set.
    NoTerminal,
    /// The session argument at this position (the program is 0) holds a control character — or,
    /// on Windows, is empty or holds a `"`.
    ControlChar(usize),
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LaunchError::NoTerminal => write!(
                f,
                "no terminal found — set AISBX_TERMINAL to the command that opens a terminal \
                 window running the words after it (for example: xterm -e)"
            ),
            LaunchError::ControlChar(i) => write!(
                f,
                "argument {i} of the session command contains a control character (on Windows: is empty \
                 or contains a \") — not launched"
            ),
        }
    }
}

impl std::error::Error for LaunchError {}

/// The AppleScript that runs `line` (already escaped) in one Terminal.app window. A Terminal.app
/// that is not running opens its own window as it starts, so the line goes into that window.
fn macos_script(line: &str) -> Vec<String> {
    let script = format!("do script \"{line}\"");
    [
        "set wasRunning to application \"Terminal\" is running",
        "tell application \"Terminal\"",
        "if not wasRunning then",
        "repeat 50 times",
        "if (count of windows) > 0 then exit repeat",
        "delay 0.1",
        "end repeat",
        "end if",
        "if not wasRunning and (count of windows) > 0 then",
        &format!("{script} in window 1"),
        "else",
        &script,
        "end if",
        "activate",
        "end tell",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The argv that opens a terminal window running `session` on `os`, the session wrapped so the
/// window waits for Enter after a failure. Pure: nothing is started.
pub fn launch_argv(os: Os, session: &[String], env: &TermEnv) -> Result<Vec<String>, LaunchError> {
    if let Some(i) = refused_at(os, session) {
        return Err(LaunchError::ControlChar(i));
    }
    let wrapped = match os {
        Os::Windows => windows_wrapped(session),
        Os::MacOs | Os::Linux => unix_wrapped(session),
    };
    let own: Vec<String> = env
        .aisbx_terminal
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    if !own.is_empty() {
        return Ok([own, wrapped].concat());
    }
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match os {
        Os::MacOs => {
            let line = applescript_escape(&posix_line(&wrapped));
            let mut argv = vec!["osascript".to_string()];
            for statement in macos_script(&line) {
                argv.extend(["-e".to_string(), statement]);
            }
            Ok(argv)
        }
        Os::Linux => LINUX_TERMINALS
            .iter()
            .find(|(program, _)| on_path(os, program, env).is_some())
            .map(|(program, args)| [words(&[program]), words(args), wrapped.clone()].concat())
            .ok_or(LaunchError::NoTerminal),
        Os::Windows => Ok([
            match on_path(os, "wt", env) {
                Some(wt) => words(&[wt.as_str(), "new-tab", "--title", "aisbx", "--"]),
                // `start` takes a quoted first word as the title; `add_args` passes it raw.
                None => words(&["cmd.exe", "/c", "start", "\"aisbx\""]),
            },
            wrapped,
        ]
        .concat()),
    }
}

/// The position of the first argument `os` will not launch ([`LaunchError::ControlChar`]).
pub fn refused_at(os: Os, argv: &[String]) -> Option<usize> {
    let windows = os == Os::Windows;
    argv.iter().position(|a| {
        a.chars().any(char::is_control) || (windows && (a.is_empty() || a.contains('"')))
    })
}

/// Start `argv` directly, with no terminal window, for an editor's own launcher. The program is
/// looked up on PATH like a terminal. The outer error is the refusal ([`refused_at`]), the inner
/// one a failed start. Under test nothing is started; the argv is recorded ([`opened`]).
pub fn start_detached(
    os: Os,
    argv: &[String],
    env: &TermEnv,
    notice: Sender<(bool, String)>,
) -> Result<io::Result<()>, LaunchError> {
    if let Some(i) = refused_at(os, argv) {
        return Err(LaunchError::ControlChar(i));
    }
    let mut argv = argv.to_vec();
    if let Some(program) = argv.first_mut() {
        if let Some(found) = on_path(os, program, env) {
            *program = found;
        }
    }
    Ok(spawn(&argv, "", true, notice))
}

/// The editor that opens `path` in a terminal: the user's ([`TermEnv::editor`], its words, so
/// `code -w` works), else `vi`, or Notepad on Windows.
pub fn editor_argv(os: Os, path: &Path, editor: Option<&str>) -> Vec<String> {
    let named: Vec<String> = editor
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut argv = if named.is_empty() {
        vec![match os {
            Os::Windows => "notepad".to_string(),
            Os::MacOs | Os::Linux => "vi".to_string(),
        }]
    } else {
        named
    };
    argv.push(path.display().to_string());
    argv
}

/// The argv that opens `path` in the desktop's own text editor, with no terminal.
pub fn open_file_argv(os: Os, path: &Path) -> Vec<String> {
    let path = path.display().to_string();
    match os {
        Os::MacOs => vec!["open".into(), "-t".into(), path],
        Os::Linux => vec!["xdg-open".into(), path],
        Os::Windows => vec!["notepad".into(), path],
    }
}

/// The session as a user would type it on `os` — for the *No terminal found* dialog.
pub fn display_command(os: Os, session: &[String]) -> String {
    match os {
        Os::Windows => format!("& {}", ps_line(session)),
        Os::MacOs | Os::Linux => posix_line(session),
    }
}

fn unix_wrapped(session: &[String]) -> Vec<String> {
    let head = ["sh", "-c", UNIX_WRAPPER, WRAPPER_NAME].map(String::from);
    [head.as_slice(), session].concat()
}

/// The PowerShell script the Windows wrapper runs: the session, then a wait after a failure.
fn ps_script(session: &[String]) -> String {
    format!(
        "& {}; $s = $LASTEXITCODE; if ($null -eq $s) {{ $s = 1 }}; if ($s -ne 0) {{ \
         Write-Host ''; Write-Host \"aisbx: exited with $s - press Enter to close\"; \
         [void](Read-Host) }}; exit $s",
        ps_line(session)
    )
}

fn windows_wrapped(session: &[String]) -> Vec<String> {
    let utf16: Vec<u8> = ps_script(session)
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    ["powershell.exe", "-NoProfile", "-EncodedCommand"]
        .map(String::from)
        .into_iter()
        .chain([base64(&utf16)])
        .collect()
}

/// The name `program` is found under in a folder on PATH — on Windows with a `PATHEXT`
/// extension, which is then the file to run.
pub(crate) fn on_path(os: Os, program: &str, env: &TermEnv) -> Option<String> {
    let path = env.path.as_ref()?;
    let path = path.to_string_lossy();
    let (sep, names) = match os {
        Os::Windows => {
            let exts = env
                .pathext
                .as_ref()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".into());
            let names = exts
                .split(';')
                .filter(|e| !e.is_empty())
                .map(|e| format!("{program}{}", e.to_ascii_lowercase()))
                .collect();
            (';', names)
        }
        Os::MacOs | Os::Linux => (':', vec![program.to_string()]),
    };
    path.split(sep)
        .filter(|dir| !dir.is_empty())
        .find_map(|dir| {
            names
                .iter()
                .find(|n| (env.exists)(&PathBuf::from(dir).join(n)))
                .cloned()
        })
}

fn posix_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| posix_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

fn ps_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| ps_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One POSIX shell word: bare when it is only safe characters, else single-quoted.
fn posix_quote(s: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "_./:=@%+,-".contains(c);
    if !s.is_empty() && s.chars().all(safe) {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// One PowerShell word in single quotes; the typographic single quotes are quotes there too.
fn ps_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
            out.push(c);
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// The inside of an AppleScript string literal.
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Standard base64 with padding, for `-EncodedCommand`.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

thread_local! {
    /// What [`open`] was asked to start under test, where it starts nothing.
    static OPENED: RefCell<Vec<Vec<String>>> = const { RefCell::new(Vec::new()) };
}

/// Take what [`open`] recorded on this thread instead of starting it.
#[cfg(any(test, feature = "test-support"))]
pub fn opened() -> Vec<Vec<String>> {
    OPENED.with(|o| std::mem::take(&mut *o.borrow_mut()))
}

/// Start the terminal with null stdio. A non-zero exit within three seconds goes to `notice` as
/// an error. Under test nothing is started; the argv is recorded ([`opened`]).
pub fn open(argv: &[String], notice: Sender<(bool, String)>) -> io::Result<()> {
    spawn(argv, "terminal ", false, notice)
}

/// The start that [`open`] and [`start_detached`] share. With `no_console`, a Windows console
/// program gets no window of its own.
fn spawn(
    argv: &[String],
    kind: &'static str,
    no_console: bool,
    notice: Sender<(bool, String)>,
) -> io::Result<()> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("no {kind}command")))?;
    if cfg!(any(test, feature = "test-support")) {
        OPENED.with(|o| o.borrow_mut().push(argv.to_vec()));
        return Ok(());
    }
    let mut command = Command::new(program);
    add_args(&mut command, program, args, no_console);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let (program, started) = (program.clone(), Instant::now());
    thread::spawn(move || {
        let Ok(status) = child.wait() else { return };
        if !status.success() && started.elapsed() < QUICK_FAILURE {
            let code = status
                .code()
                .map_or_else(|| "a signal".to_string(), |c| c.to_string());
            let _ = notice.send((true, format!("{kind}`{program}` exited with {code}")));
        }
    });
    Ok(())
}

/// `cmd.exe` parses its own command line: raw arguments keep the quotes of `start "aisbx"`.
/// `CREATE_NO_WINDOW` stops a `.cmd` launcher from opening a console; null stdio does not.
#[cfg(windows)]
fn add_args(command: &mut Command, program: &str, args: &[String], no_console: bool) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    if no_console {
        command.creation_flags(CREATE_NO_WINDOW);
    }
    if program.eq_ignore_ascii_case("cmd.exe") {
        for arg in args {
            command.raw_arg(arg);
        }
    } else {
        command.args(args);
    }
}

#[cfg(not(windows))]
fn add_args(command: &mut Command, _program: &str, args: &[String], _no_console: bool) {
    command.args(args);
}

/// A session being prepared: the handoff's pre-start steps, running on a background thread.
pub struct Launch {
    handoff: Handoff,
    log: Arc<Mutex<Vec<String>>>,
    progress: Arc<Mutex<Option<Progress>>>,
    finished: Arc<AtomicBool>,
}

impl Launch {
    /// Run `handoff`'s prelude in order, each step to completion, none able to stop the session.
    /// Under test no step runs and the launch is finished at once.
    pub fn start(handoff: Handoff) -> Self {
        let steps = if cfg!(any(test, feature = "test-support")) {
            Vec::new()
        } else {
            handoff.prelude().to_vec()
        };
        let launch = Launch {
            handoff,
            log: Arc::default(),
            progress: Arc::default(),
            finished: Arc::new(AtomicBool::new(steps.is_empty())),
        };
        if steps.is_empty() {
            return launch;
        }
        let (log, progress, finished) = (
            Arc::clone(&launch.log),
            Arc::clone(&launch.progress),
            Arc::clone(&launch.finished),
        );
        thread::spawn(move || {
            let total = steps.len();
            for (i, argv) in steps.iter().enumerate() {
                let label = step_label(argv);
                *progress.lock().unwrap() = Some(Progress {
                    index: i + 1,
                    total,
                    label: label.clone(),
                    note: None,
                });
                push(&log, format!("$ {}", argv.join(" ")));
                match run_step(argv, &log) {
                    Ok(0) => {}
                    Ok(code) => push(
                        &log,
                        format!("pre-start step {label} exited with {code} — starting anyway"),
                    ),
                    Err(e) => push(
                        &log,
                        format!("pre-start step {label} could not start ({e}) — starting anyway"),
                    ),
                }
            }
            finished.store(true, Ordering::SeqCst);
        });
        launch
    }

    /// The step running now, `None` before the first and when there is none.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().unwrap().clone()
    }

    pub fn lines(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    pub fn handoff(&self) -> &Handoff {
        &self.handoff
    }
}

/// A step's argv, short enough for one line of the preparing strip.
fn step_label(argv: &[String]) -> String {
    let line = argv.join(" ");
    let first: String = line
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(60)
        .collect();
    if first.len() < line.len() {
        format!("{first}…")
    } else {
        first
    }
}

/// Run one step with stdin closed, its output into `log`. The exit code, or -1 for a signal.
fn run_step(argv: &[String], log: &Arc<Mutex<Vec<String>>>) -> io::Result<i32> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|err| {
        let log = Arc::clone(log);
        thread::spawn(move || pump(err, &log))
    });
    if let Some(out) = child.stdout.take() {
        pump(out, log);
    }
    if let Some(t) = stderr {
        let _ = t.join();
    }
    Ok(child.wait()?.code().unwrap_or(-1))
}

fn pump(reader: impl Read, log: &Arc<Mutex<Vec<String>>>) {
    for line in BufReader::new(reader).split(b'\n').map_while(Result::ok) {
        let line: String = String::from_utf8_lossy(&line)
            .chars()
            .filter(|c| !c.is_control() || *c == '\t')
            .collect();
        if !line.trim().is_empty() {
            push(log, line);
        }
    }
}

fn push(log: &Arc<Mutex<Vec<String>>>, line: String) {
    let mut log = log.lock().unwrap();
    if log.len() >= 2000 {
        log.drain(0..500);
    }
    log.push(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_opens_in_the_named_editor_else_the_platforms() {
        let path = Path::new("/s/edited-opencode/a b.json");
        let argv = |os, editor| editor_argv(os, path, editor);
        assert_eq!(
            argv(Os::Linux, Some(" code  -w ")),
            ["code", "-w", "/s/edited-opencode/a b.json"]
        );
        assert_eq!(argv(Os::Linux, Some("  "))[0], "vi");
        assert_eq!(argv(Os::MacOs, None)[0], "vi");
        assert_eq!(argv(Os::Windows, None)[0], "notepad");
        assert_eq!(open_file_argv(Os::MacOs, path)[..2], ["open", "-t"]);
        assert_eq!(open_file_argv(Os::Linux, path)[0], "xdg-open");
        assert_eq!(open_file_argv(Os::Windows, path)[0], "notepad");
    }

    fn strings(words: &[&str]) -> Vec<String> {
        words.iter().map(|s| s.to_string()).collect()
    }

    /// Space-separated words, for argv without spaces in them.
    fn words(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    fn env<'a>(path: &str, exists: &'a dyn Fn(&Path) -> bool) -> TermEnv<'a> {
        let (aisbx_terminal, path, pathext) = (None, Some(path.into()), None);
        TermEnv {
            aisbx_terminal,
            path,
            pathext,
            exists,
            editor: None,
        }
    }

    const NOTHING: &dyn Fn(&Path) -> bool = &|_| false;

    /// Every awkward character a session argument can carry.
    const AWKWARD: [&str; 8] = [
        "a b",
        "it's",
        "say \"hi\"",
        "back\\slash",
        "$HOME",
        "`id`",
        "a;b",
        "",
    ];

    fn awkward() -> Vec<String> {
        [words("sbx exec"), strings(&AWKWARD)].concat()
    }

    fn unwrap_wrapper(session: Vec<String>) -> Vec<String> {
        [strings(&["sh", "-c", UNIX_WRAPPER, WRAPPER_NAME]), session].concat()
    }

    fn decode_base64(s: &str) -> Vec<u8> {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let value = |c: u8| alphabet.iter().position(|a| *a == c).unwrap() as u32;
        let digits: Vec<u32> = s.bytes().filter(|c| *c != b'=').map(value).collect();
        let mut out = Vec::new();
        for quad in digits.chunks(4) {
            let n = quad.iter().fold(0, |n, v| n << 6 | v) << (6 * (4 - quad.len()));
            out.extend((0..quad.len() - 1).map(|i| (n >> (16 - 8 * i)) as u8));
        }
        out
    }

    #[test]
    fn base64_matches_the_reference_vectors() {
        let vectors = [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v")];
        for (input, expected) in vectors.into_iter().chain([("foobar", "Zm9vYmFy")]) {
            assert_eq!(base64(input.as_bytes()), expected, "{input}");
            assert_eq!(decode_base64(expected), input.as_bytes());
        }
    }

    #[test]
    fn quoting_keeps_every_awkward_character_literal() {
        for (raw, quoted) in [
            ("sbx", "sbx"),
            ("--name=x/y:1", "--name=x/y:1"),
            ("", "''"),
            ("a b", "'a b'"),
            ("it's", "'it'\\''s'"),
            ("$HOME", "'$HOME'"),
            ("`id`", "'`id`'"),
            ("say \"hi\" back\\slash", "'say \"hi\" back\\slash'"),
        ] {
            assert_eq!(posix_quote(raw), quoted);
        }
        assert_eq!(
            ps_quote("it's $HOME `id` \"x\""),
            "'it''s $HOME `id` \"x\"'"
        );
        let typographic = "\u{2018}a\u{2019}b\u{201a}c\u{201b}";
        let doubled = "'\u{2018}\u{2018}a\u{2019}\u{2019}b\u{201a}\u{201a}c\u{201b}\u{201b}'";
        assert_eq!(ps_quote(typographic), doubled);
        assert_eq!(applescript_escape("a\\b\"c"), "a\\\\b\\\"c");
    }

    #[test]
    fn a_control_character_is_refused_by_position() {
        let session = strings(&["sbx", "run", "--name", "x\ny"]);
        for os in [Os::MacOs, Os::Linux, Os::Windows] {
            let err = launch_argv(os, &session, &env("/usr/bin", &|_| true)).unwrap_err();
            assert_eq!(err, LaunchError::ControlChar(3));
            assert!(!err.to_string().contains("x\ny") && err.to_string().contains('3'));
        }
        let nul = launch_argv(Os::Linux, &strings(&["sbx", "a\0"]), &env("", NOTHING));
        assert_eq!(nul, Err(LaunchError::ControlChar(1)));
    }

    #[test]
    fn windows_refuses_an_empty_argument_and_a_double_quote_by_position() {
        for (session, at) in [(&["sbx", "run", ""][..], 2), (&["sbx", "say \"hi\""], 1)] {
            let session = strings(session);
            let err = launch_argv(Os::Windows, &session, &env("", NOTHING)).unwrap_err();
            assert_eq!(err, LaunchError::ControlChar(at));
            assert!(
                err.to_string().contains(&format!("argument {at} ")),
                "{err}"
            );
            for os in [Os::MacOs, Os::Linux] {
                assert!(launch_argv(os, &session, &env("/usr/bin", &|_| true)).is_ok());
            }
        }
    }

    #[test]
    fn linux_takes_the_first_terminal_on_path_in_order() {
        let session = words("sbx run --name box");
        let run = |path: &str, exists: &dyn Fn(&Path) -> bool| {
            launch_argv(Os::Linux, &session, &env(path, exists))
        };
        let found =
            |p: &Path| p == Path::new("/opt/bin/konsole") || p == Path::new("/usr/bin/xterm");
        let wrapped = unwrap_wrapper(session.clone());
        let expect = |head: &str| Ok([words(head), wrapped.clone()].concat());
        assert_eq!(run("/usr/bin::/opt/bin", &found), expect("konsole -e"));
        let gnome = run("/usr/bin", &|p| p.ends_with("gnome-terminal"));
        assert_eq!(gnome, expect("gnome-terminal --"));
        assert_eq!(run("/usr/bin", &|p| p.ends_with("kitty")), expect("kitty"));
        assert_eq!(run("/usr/bin", NOTHING), Err(LaunchError::NoTerminal));
        let no_path = TermEnv {
            path: None,
            ..env("", &|_| true)
        };
        let none = launch_argv(Os::Linux, &session, &no_path);
        assert_eq!(none, Err(LaunchError::NoTerminal));
        assert!(LaunchError::NoTerminal
            .to_string()
            .contains("AISBX_TERMINAL"));
    }

    #[test]
    fn aisbx_terminal_overrides_the_detection() {
        let session = words("sbx run --name box");
        let run = |os, value: &str| {
            let own = TermEnv {
                aisbx_terminal: Some(value.into()),
                ..env("/usr/bin", &|_| true)
            };
            launch_argv(os, &session, &own).unwrap()
        };
        let argv = run(Os::Linux, "  footclient  -e ");
        assert_eq!(argv[..3], words("footclient -e sh"));
        assert_eq!(argv[argv.len() - 4..], session[..]);
        assert_eq!(
            run(Os::MacOs, "open -a iTerm")[..4],
            words("open -a iTerm sh")
        );
        let windows = words("alacritty.exe -e powershell.exe");
        assert_eq!(run(Os::Windows, "alacritty.exe -e")[..3], windows);
        assert_eq!(run(Os::Linux, "   ")[0], "x-terminal-emulator");
    }

    #[test]
    fn macos_drives_terminal_app_with_an_escaped_quoted_line() {
        let argv = launch_argv(Os::MacOs, &awkward(), &env("", NOTHING)).unwrap();
        assert_eq!(argv[0], "osascript");
        let statements: Vec<&String> = argv[1..].iter().skip(1).step_by(2).collect();
        assert!(argv[1..].iter().step_by(2).all(|flag| flag == "-e"));
        let scripts: Vec<&&String> = statements
            .iter()
            .filter(|s| s.starts_with("do script"))
            .collect();
        assert_eq!(
            scripts.len(),
            2,
            "one for a new window, one for Terminal's own"
        );
        assert!(scripts[0].ends_with("\" in window 1"));
        let prefix = "do script \"";
        let inner = scripts[1]
            .strip_prefix(prefix)
            .and_then(|s| s.strip_suffix('"'));
        let inner = inner.unwrap();
        assert_eq!(*scripts[0], &format!("{} in window 1", scripts[1]));
        let unescaped = inner.replace("\\\"", "");
        assert!(!unescaped.contains('"'), "an unescaped quote: {inner}");
        let line = inner.replace("\\\"", "\"").replace("\\\\", "\\");
        assert_eq!(line, posix_line(&unwrap_wrapper(awkward())));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn the_macos_script_compiles() {
        let argv = launch_argv(Os::MacOs, &awkward(), &env("", NOTHING)).unwrap();
        let out = std::env::temp_dir().join(format!("aisbx-tests-{}.scpt", std::process::id()));
        let status = Command::new("osacompile")
            .args(&argv[1..])
            .arg("-o")
            .arg(&out)
            .status()
            .unwrap();
        let _ = std::fs::remove_file(&out);
        assert!(status.success());
    }

    #[test]
    #[cfg(unix)]
    fn the_posix_line_round_trips_through_a_real_shell() {
        let line = format!("printf '%s\\n' {}", posix_line(&awkward()));
        let out = Command::new("sh").arg("-c").arg(&line).output().unwrap();
        let expected: String = awkward().iter().map(|w| format!("{w}\n")).collect();
        assert_eq!(String::from_utf8(out.stdout).unwrap(), expected);
    }

    #[test]
    fn windows_prefers_windows_terminal_and_falls_back_to_cmd_start() {
        let mut session = awkward();
        session.retain(|a| !a.is_empty() && !a.contains('"'));
        let wt = |p: &Path| {
            p.file_name() == Some("wt.exe".as_ref())
                && p.parent()
                    .is_some_and(|d| d.to_string_lossy().ends_with("WindowsApps"))
        };
        let found = env(
            "C:\\Windows;C:\\Users\\me\\AppData\\Local\\Microsoft\\WindowsApps",
            &wt,
        );
        let argv = launch_argv(Os::Windows, &session, &found).unwrap();
        let shell = "powershell.exe -NoProfile -EncodedCommand";
        let head = words(&format!("wt.exe new-tab --title aisbx -- {shell}"));
        assert_eq!((&argv[..8], argv.len()), (&head[..], 9));
        let only = |exists| TermEnv {
            pathext: Some(".COM".into()),
            ..env("C:\\bin", exists)
        };
        let com = |p: &Path| p.to_string_lossy().ends_with("wt.com");
        let argv = launch_argv(Os::Windows, &session, &only(&com)).unwrap();
        assert_eq!(argv[0], "wt.com");
        let argv = launch_argv(Os::Windows, &session, &only(&wt)).unwrap();
        let head = words(&format!("cmd.exe /c start \"aisbx\" {shell}"));
        assert_eq!(argv[..7], head);
        let base64 = |c: char| c.is_ascii_alphanumeric() || "+/=".contains(c);
        assert!(argv.last().unwrap().chars().all(base64));
    }

    #[test]
    fn the_encoded_command_decodes_back_to_the_script() {
        let session = strings(&["sbx", "run", "--name", "it's \u{2019}box\u{2019}", "$x"]);
        let argv = launch_argv(Os::Windows, &session, &env("", NOTHING)).unwrap();
        let bytes = decode_base64(argv.last().unwrap());
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        let script = String::from_utf16(&units).unwrap();
        assert_eq!(script, ps_script(&session));
        let head = "& 'sbx' 'run' '--name' 'it''s \u{2019}\u{2019}box\u{2019}\u{2019}' '$x'; ";
        assert!(script.starts_with(head), "{script}");
        assert!(script.contains("Read-Host") && script.contains("exit $s"));
    }

    #[test]
    fn the_display_command_is_quoted_for_the_host() {
        let session = strings(&["sbx", "exec", "-it", "my box", "bash"]);
        for os in [Os::Linux, Os::MacOs] {
            assert_eq!(display_command(os, &session), "sbx exec -it 'my box' bash");
        }
        let windows = "& 'sbx' 'exec' '-it' 'my box' 'bash'";
        assert_eq!(display_command(Os::Windows, &session), windows);
    }

    /// Run the Unix wrapper as the terminal would, with `stdin` as what the user types.
    #[cfg(unix)]
    fn run_wrapper(session: &[&str], stdin: &str) -> (i32, String) {
        use std::io::Write;
        let argv = unix_wrapped(&strings(session));
        let mut command = Command::new(&argv[0]);
        let piped = command.args(&argv[1..]).stdin(Stdio::piped());
        let mut child = piped.stdout(Stdio::piped()).spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.as_bytes()).unwrap();
        drop(input);
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8(out.stdout).unwrap();
        (out.status.code().unwrap(), stdout)
    }

    #[test]
    #[cfg(unix)]
    fn the_unix_wrapper_passes_arguments_and_exit_code_and_waits_only_on_failure() {
        let session = [["printf", "[%s]\\n"].as_slice(), &AWKWARD].concat();
        let expected: String = AWKWARD.iter().map(|w| format!("[{w}]\n")).collect();
        assert_eq!(run_wrapper(&session, ""), (0, expected));
        let (code, out) = run_wrapper(&["sh", "-c", "echo failing; exit 7"], "\n");
        let asked = out.contains("failing\n\naisbx: exited with 7 - press Enter to close");
        assert!(code == 7 && asked, "{code} {out}");
        assert_eq!(run_wrapper(&["true"], ""), (0, String::new()));
    }

    #[test]
    fn under_test_a_launch_runs_nothing_and_open_only_records() {
        let prelude = vec![words("false")];
        let handoff = Handoff::Agent {
            name: "box".into(),
            start: None,
            prelude,
        };
        let launch = Launch::start(handoff.clone());
        assert!(launch.is_finished() && launch.progress().is_none() && launch.lines().is_empty());
        assert_eq!(launch.handoff(), &handoff);

        let (tx, _rx) = std::sync::mpsc::channel();
        open(&words("xterm -e sh"), tx.clone()).unwrap();
        assert_eq!(opened(), vec![words("xterm -e sh")]);
        assert!(opened().is_empty(), "taken");
        assert!(open(&[], tx).is_err());
    }

    #[test]
    fn refused_at_is_shared_by_the_terminal_and_the_detached_start() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let cases: [(&[&str], [Option<usize>; 3]); 4] = [
            (&["code", "--remote", "/p"], [None, None, None]),
            (&["code", "a\tb"], [Some(1), Some(1), Some(1)]),
            (&["code", ""], [None, None, Some(1)]),
            (&["code", "/p", "say \"hi\""], [None, None, Some(2)]),
        ];
        for (argv, want) in cases {
            let argv = strings(argv);
            for (os, want) in [Os::MacOs, Os::Linux, Os::Windows].into_iter().zip(want) {
                assert_eq!(refused_at(os, &argv), want, "{os:?} {argv:?}");
                let env = env("/usr/bin", &|_| true);
                let terminal = launch_argv(os, &argv, &env).err();
                let detached = start_detached(os, &argv, &env, tx.clone()).err();
                assert_eq!(terminal, want.map(LaunchError::ControlChar), "{os:?}");
                assert_eq!(detached, terminal, "{os:?} {argv:?}");
            }
        }
        opened();
    }

    #[test]
    fn a_detached_start_records_the_bare_argv_with_the_program_found_on_path() {
        let (tx, _rx) = std::sync::mpsc::channel();
        let argv = words("code --remote ssh-remote+box.sbx /p");
        let cmd = |p: &Path| p.ends_with("code.cmd");
        start_detached(Os::Windows, &argv, &env("C:\\bin", &cmd), tx.clone())
            .unwrap()
            .unwrap();
        start_detached(Os::Linux, &argv, &env("/usr/bin", &|_| true), tx.clone())
            .unwrap()
            .unwrap();
        start_detached(Os::Linux, &argv, &env("", NOTHING), tx.clone())
            .unwrap()
            .unwrap();
        let windows = [strings(&["code.cmd"]), argv[1..].to_vec()].concat();
        assert_eq!(opened(), vec![windows, argv.clone(), argv]);
        let exe = |p: &Path| p.ends_with("ssh.exe");
        let found = on_path(Os::Windows, "ssh", &env("C:\\OpenSSH", &exe));
        assert_eq!(found.as_deref(), Some("ssh.exe"));
        assert!(start_detached(Os::Linux, &[], &env("", NOTHING), tx)
            .unwrap()
            .is_err());
    }

    #[test]
    fn a_step_label_fits_one_line() {
        assert_eq!(step_label(&words("sbx stop x")), "sbx stop x");
        let long = step_label(&[words("sbx exec x sh -c"), vec!["y".repeat(100)]].concat());
        assert!(long.ends_with('…') && long.chars().count() == 61, "{long}");
        assert_eq!(step_label(&strings(&["sh", "-c", "a\nb"])), "sh -c a…");
    }
}

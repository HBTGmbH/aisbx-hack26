//! Runs a sequence of `sbx` commands off the UI thread, streaming their output into a modal.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// One command in a job, with the headline shown above its output.
#[derive(Clone)]
pub struct Step {
    pub label: String,
    pub argv: Vec<String>,
    /// Written to the command's stdin, then the pipe is closed. It carries a secret, so it is
    /// never logged.
    pub stdin: Option<String>,
    /// One line under the status while this step runs, for a step whose *pauses* need explaining.
    pub note: Option<String>,
}

/// Redacts [`Step::stdin`], so a stray `{:?}` of a `PendingJob` cannot print a token.
impl std::fmt::Debug for Step {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Step")
            .field("label", &self.label)
            .field("argv", &self.argv)
            .field("stdin", &self.stdin.as_ref().map(|_| "<redacted>"))
            .field("note", &self.note)
            .finish()
    }
}

impl Step {
    pub fn new(label: impl Into<String>, argv: Vec<String>) -> Self {
        Self {
            label: label.into(),
            argv,
            stdin: None,
            note: None,
        }
    }

    /// A step that feeds `secret` to the command on stdin instead of passing it as an argument.
    pub fn with_stdin(label: impl Into<String>, argv: Vec<String>, secret: String) -> Self {
        Self {
            label: label.into(),
            argv,
            stdin: Some(secret),
            note: None,
        }
    }

    /// Say what this step is doing while it runs — see [`Step::note`].
    pub fn noting(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

/// Which step a running job is on, for the status line under its log.
#[derive(Debug, Clone)]
pub struct Progress {
    /// 1-based, as the status line shows it ("2/3").
    pub index: usize,
    pub total: usize,
    pub label: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Running,
    Ok,
    Failed(String),
}

/// Where one step of a job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Not started yet: a step before it is still running, or failed.
    Waiting,
    Running,
    Done,
    Failed,
}

/// One step of a job as it stands, for a frontend that shows the job step by step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepView {
    pub label: String,
    /// The command line, as the log writes it after `$ `.
    pub command: String,
    pub state: StepState,
    /// What the command printed, stdout and stderr in the order they arrived and without ANSI
    /// escapes, then `error: …` when it could not be started.
    pub output: Vec<String>,
    /// How long it ran, or has run so far; `None` while it waits.
    pub elapsed: Option<Duration>,
}

/// One step's part of a job, as the job's thread records it.
#[derive(Debug)]
struct Record {
    label: String,
    command: String,
    output: Vec<String>,
    started: Option<Instant>,
    ended: Option<Instant>,
    failed: bool,
}

impl Record {
    fn of(step: &Step) -> Self {
        Self {
            label: step.label.clone(),
            command: step.argv.join(" "),
            output: Vec::new(),
            started: None,
            ended: None,
            failed: false,
        }
    }

    fn state(&self) -> StepState {
        match (self.started, self.ended, self.failed) {
            (None, _, _) => StepState::Waiting,
            (Some(_), None, _) => StepState::Running,
            (Some(_), Some(_), true) => StepState::Failed,
            (Some(_), Some(_), false) => StepState::Done,
        }
    }
}

type Records = Arc<Mutex<Vec<Record>>>;

#[derive(Debug)]
pub struct Job {
    pub title: String,
    /// One record per step, all made at the start, so a frontend can name the steps to come.
    records: Records,
    status: Arc<Mutex<Status>>,
    progress: Arc<Mutex<Option<Progress>>>,
    finished: Arc<AtomicBool>,
    started: Instant,
    ended: Arc<Mutex<Option<Instant>>>,
    /// The moment a [`Job::frozen`] job stands still at: its clocks read this, not the time
    /// now, so a test sees the times it gave. `None` for a job that runs.
    clock: Option<Instant>,
    /// Scroll offset into the log, owned by the UI.
    pub scroll: u16,
    pub follow: bool,
}

impl Job {
    pub fn spawn(title: impl Into<String>, steps: Vec<Step>) -> Self {
        let records: Records = Arc::new(Mutex::new(steps.iter().map(Record::of).collect()));
        let status = Arc::new(Mutex::new(Status::Running));
        let progress = Arc::new(Mutex::new(None));
        let finished = Arc::new(AtomicBool::new(false));
        let ended = Arc::new(Mutex::new(None));

        let job = Self {
            title: title.into(),
            records: Arc::clone(&records),
            status: Arc::clone(&status),
            progress: Arc::clone(&progress),
            finished: Arc::clone(&finished),
            started: Instant::now(),
            ended: Arc::clone(&ended),
            clock: None,
            scroll: 0,
            follow: true,
        };

        thread::spawn(move || {
            let total = steps.len();
            for (i, step) in steps.into_iter().enumerate() {
                *progress.lock().unwrap() = Some(Progress {
                    index: i + 1,
                    total,
                    label: step.label.clone(),
                    note: step.note.clone(),
                });
                records.lock().unwrap()[i].started = Some(Instant::now());
                let failure = match run_step(&step, &records, i) {
                    Ok(exit) if exit.success() => None,
                    Ok(exit) => Some(format!("{} failed ({})", step.label, exit_reason(exit))),
                    Err(e) => {
                        push(&records, i, format!("error: {e}"));
                        Some(e)
                    }
                };
                {
                    let mut records = records.lock().unwrap();
                    records[i].ended = Some(Instant::now());
                    records[i].failed = failure.is_some();
                }
                if let Some(why) = failure {
                    *status.lock().unwrap() = Status::Failed(why);
                    *ended.lock().unwrap() = Some(Instant::now());
                    finished.store(true, Ordering::SeqCst);
                    return;
                }
            }
            *status.lock().unwrap() = Status::Ok;
            *ended.lock().unwrap() = Some(Instant::now());
            finished.store(true, Ordering::SeqCst);
        });

        job
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    /// Which step is running, or `None` before the first one starts. Stale once the job ends.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().unwrap().clone()
    }

    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::SeqCst)
    }

    /// The log as one text: for each step that started, `── label`, `$ command`, what it printed,
    /// and a blank line once it succeeded.
    pub fn lines(&self) -> Vec<String> {
        let records = self.records.lock().unwrap();
        let mut lines = Vec::new();
        for record in records.iter().filter(|r| r.started.is_some()) {
            lines.push(format!("── {}", record.label));
            lines.push(format!("$ {}", record.command));
            lines.extend(record.output.iter().cloned());
            if record.state() == StepState::Done {
                lines.push(String::new());
            }
        }
        lines
    }

    /// Every step, the ones still to come included, as it stands now.
    pub fn steps(&self) -> Vec<StepView> {
        let now = self.now();
        let records = self.records.lock().unwrap();
        (records.iter())
            .map(|r| StepView {
                label: r.label.clone(),
                command: r.command.clone(),
                state: r.state(),
                output: r.output.clone(),
                elapsed: r
                    .started
                    .map(|at| r.ended.unwrap_or(now).saturating_duration_since(at)),
            })
            .collect()
    }

    /// How long the job has run, or ran.
    pub fn elapsed(&self) -> Duration {
        let end = self.ended.lock().unwrap().unwrap_or_else(|| self.now());
        end.saturating_duration_since(self.started)
    }

    /// The time the job's clocks read: now, or the moment a frozen job stands still at.
    fn now(&self) -> Instant {
        self.clock.unwrap_or_else(Instant::now)
    }
}

/// A job frozen where its steps say it is, for render tests: nothing runs. It failed if a step
/// did, succeeded if every step is done, and runs otherwise.
#[cfg(any(test, feature = "test-support"))]
impl Job {
    pub fn frozen(title: impl Into<String>, steps: Vec<StepView>, note: Option<&str>) -> Self {
        let now = Instant::now();
        let ago = |d: Option<Duration>| now.checked_sub(d.unwrap_or_default()).unwrap_or(now);
        let total = steps.len();
        let at =
            (steps.iter()).position(|s| matches!(s.state, StepState::Running | StepState::Failed));
        let progress = at.map(|i| Progress {
            index: i + 1,
            total,
            label: steps[i].label.clone(),
            note: note.map(str::to_string),
        });
        let failed = steps.iter().find(|s| s.state == StepState::Failed);
        let status = match (failed, at) {
            (Some(step), _) => Status::Failed(format!("{} failed (exit code 1)", step.label)),
            (None, Some(_)) => Status::Running,
            (None, None) => Status::Ok,
        };
        let spent: Duration = steps.iter().filter_map(|s| s.elapsed).sum();
        let records = (steps.iter())
            .map(|s| Record {
                label: s.label.clone(),
                command: s.command.clone(),
                output: s.output.clone(),
                started: s.elapsed.map(|d| ago(Some(d))),
                ended: match s.state {
                    StepState::Done | StepState::Failed => Some(now),
                    StepState::Running | StepState::Waiting => None,
                },
                failed: s.state == StepState::Failed,
            })
            .collect();
        let finished = status != Status::Running;
        Self {
            title: title.into(),
            records: Arc::new(Mutex::new(records)),
            status: Arc::new(Mutex::new(status)),
            progress: Arc::new(Mutex::new(progress)),
            finished: Arc::new(AtomicBool::new(finished)),
            started: ago(Some(spent)),
            ended: Arc::new(Mutex::new(finished.then_some(now))),
            // Nothing runs, so no clock moves: a running step reads the time it was given.
            clock: Some(now),
            scroll: 0,
            follow: true,
        }
    }
}

/// How a command that did not succeed ended, for [`Status::Failed`].
fn exit_reason(exit: ExitStatus) -> String {
    match exit.code() {
        Some(code) => format!("exit code {code}"),
        // Only Unix has no code, for a process a signal ended.
        None => "stopped by a signal".to_string(),
    }
}

/// Run one step, its output into its record; `Err` when the command could not be run at all.
fn run_step(step: &Step, records: &Records, index: usize) -> Result<ExitStatus, String> {
    let (program, args) = step
        .argv
        .split_first()
        .ok_or_else(|| "empty command".to_string())?;

    let mut child = Command::new(program)
        .args(args)
        .stdin(if step.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start `{program}`: {e}"))?;

    let feeder = feed_stdin(step, &mut child)?;

    // Drain both pipes at once: a full stderr pipe blocks a child that still writes to stdout.
    let stderr = child.stderr.take();
    let err_records = Arc::clone(records);
    let err_thread = stderr.map(|s| thread::spawn(move || pump(s, &err_records, index)));

    if let Some(stdout) = child.stdout.take() {
        pump(stdout, records, index);
    }
    if let Some(t) = err_thread {
        let _ = t.join();
    }

    let fed = feeder.map(|t| t.join());
    let status = child
        .wait()
        .map_err(|e| format!("waiting for `{program}`: {e}"))?;
    match fed {
        Some(Ok(Err(e))) => Err(format!("writing to `{program}` stdin: {e}")),
        _ => Ok(status),
    }
}

/// Write a step's stdin from a thread of its own, then close the pipe: a large input (a script)
/// would block a child whose output nobody drains yet.
fn feed_stdin(
    step: &Step,
    child: &mut std::process::Child,
) -> Result<Option<thread::JoinHandle<std::io::Result<()>>>, String> {
    let Some(input) = step.stdin.clone() else {
        return Ok(None);
    };
    let mut pipe = child.stdin.take().ok_or("stdin pipe was not opened")?;
    Ok(Some(thread::spawn(move || writeln!(pipe, "{input}"))))
}

/// Run one step to its end with no record of its output: `true` when it exited 0. For the work
/// the dashboard does in the background, such as the start-of-day model fetch.
pub fn run_quiet(step: &Step) -> bool {
    let Some((program, args)) = step.argv.split_first() else {
        return false;
    };
    let stdin = match step.stdin.is_some() {
        true => Stdio::piped(),
        false => Stdio::null(),
    };
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(stdin)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let fed = feed_stdin(step, &mut child).ok().flatten();
    let status = child.wait();
    let fed_ok = fed.is_none_or(|t| t.join().is_ok_and(|r| r.is_ok()));
    fed_ok && status.is_ok_and(|s| s.success())
}

/// Read a pipe into step `index`'s output, splitting on `\n` *and* `\r` so progress spinners
/// land as successive lines instead of one ever-growing one, and dropping ANSI escapes.
fn pump<R: Read>(reader: R, records: &Records, index: usize) {
    let mut buf = BufReader::new(reader);
    let mut chunk: Vec<u8> = Vec::new();
    loop {
        chunk.clear();
        match buf.read_until(b'\n', &mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&chunk);
        for part in text.split(['\n', '\r']) {
            let line = strip_ansi(part);
            if !line.trim().is_empty() {
                push(records, index, line);
            }
        }
    }
}

fn push(records: &Records, index: usize, line: String) {
    let mut guard = records.lock().unwrap();
    let output = &mut guard[index].output;
    if output.len() >= 5000 {
        output.drain(0..1000);
    }
    output.push(line);
}

/// Drop CSI/OSC escape sequences so the log renders as plain text inside the modal.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            // CSI … final byte in @–~
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            // OSC … BEL or ST
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_and_osc_sequences() {
        assert_eq!(strip_ansi("\u{1b}[32mgreen\u{1b}[0m"), "green");
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}text"), "text");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn keeps_tabs_but_drops_other_controls() {
        assert_eq!(strip_ansi("a\tb\u{8}c"), "a\tbc");
    }

    fn sh(label: &str, script: &str) -> Step {
        Step::new(label, vec!["sh".into(), "-c".into(), script.into()])
    }

    fn finish(job: &Job) {
        for _ in 0..500 {
            if job.is_finished() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("the job did not finish");
    }

    #[test]
    #[cfg(unix)]
    fn a_large_stdin_reaches_a_child_that_writes_as_it_reads() {
        // `cat` echoes what it reads; 1 MiB fills both pipes unless stdin is fed apart.
        let input = "x".repeat(1 << 20);
        let mut step = sh("echo", "cat; echo end");
        step.stdin = Some(input.clone());
        let job = Job::spawn("big", vec![step.clone()]);
        finish(&job);
        assert_eq!(job.status(), Status::Ok);
        assert!(job.lines().iter().any(|l| l == "end"));
        assert!(run_quiet(&step));
        let mut failing = sh("fail", "cat >/dev/null; exit 3");
        failing.stdin = Some(input);
        assert!(!run_quiet(&failing));
        assert!(!run_quiet(&Step::new("none", Vec::new())));
    }

    #[test]
    #[cfg(unix)]
    fn a_failed_step_is_named_with_its_exit_code_not_its_command() {
        let job = Job::spawn(
            "Creating box",
            vec![
                sh("store the key", "echo stored"),
                sh("sbx create box", "echo building; exit 3"),
                sh("write the label", "true"),
            ],
        );
        finish(&job);
        assert_eq!(
            job.status(),
            Status::Failed("sbx create box failed (exit code 3)".into())
        );
        let steps = job.steps();
        let states: Vec<StepState> = steps.iter().map(|s| s.state).collect();
        assert_eq!(
            states,
            [StepState::Done, StepState::Failed, StepState::Waiting]
        );
        assert_eq!(steps[1].output, ["building"]);
        assert_eq!(steps[1].command, "sh -c echo building; exit 3");
        assert!(steps[0].elapsed.is_some() && steps[1].elapsed.is_some());
        assert_eq!(steps[2].elapsed, None, "a step that never ran has no clock");
        assert!(job.elapsed() >= steps[1].elapsed.unwrap());
    }

    #[test]
    #[cfg(unix)]
    fn the_whole_log_reads_step_after_step() {
        let job = Job::spawn(
            "Stopping",
            vec![
                sh("first", "echo one; echo two >&2"),
                sh("second", "exit 1"),
                sh("third", "true"),
            ],
        );
        finish(&job);
        let lines = job.lines();
        assert_eq!(&lines[..2], ["── first", "$ sh -c echo one; echo two >&2"]);
        let mut printed = lines[2..4].to_vec();
        printed.sort();
        assert_eq!(printed, ["one", "two"], "stdout and stderr both land");
        assert_eq!(&lines[4..], ["", "── second", "$ sh -c exit 1"]);
    }

    #[test]
    fn a_command_that_cannot_start_says_why() {
        let missing = "aisbx-no-such-program-anywhere";
        let job = Job::spawn("Missing", vec![Step::new("run it", vec![missing.into()])]);
        finish(&job);
        let Status::Failed(why) = job.status() else {
            panic!("it failed")
        };
        assert!(
            why.starts_with(&format!("could not start `{missing}`")),
            "{why}"
        );
        let steps = job.steps();
        assert_eq!(steps[0].state, StepState::Failed);
        assert!(steps[0].output[0].starts_with("error: could not start"));
    }

    #[test]
    #[cfg(unix)]
    fn a_step_a_signal_ended_says_so() {
        let job = Job::spawn("Killed", vec![sh("sleep", "kill -9 $$")]);
        finish(&job);
        assert_eq!(
            job.status(),
            Status::Failed("sleep failed (stopped by a signal)".into())
        );
    }

    #[test]
    fn a_frozen_job_is_where_its_steps_say() {
        let step = |label: &str, state, secs: Option<u64>| StepView {
            label: label.into(),
            command: format!("sbx {label}"),
            state,
            output: vec![format!("{label} says hi")],
            elapsed: secs.map(Duration::from_secs),
        };
        let running = Job::frozen(
            "Creating",
            vec![
                step("rm", StepState::Done, Some(2)),
                step("create", StepState::Running, Some(40)),
                step("label", StepState::Waiting, None),
            ],
            Some("pulling the image"),
        );
        assert_eq!(running.status(), Status::Running);
        assert!(!running.is_finished());
        let p = running.progress().unwrap();
        assert_eq!((p.index, p.total, p.label.as_str()), (2, 3, "create"));
        assert_eq!(p.note.as_deref(), Some("pulling the image"));
        assert_eq!(running.elapsed(), Duration::from_secs(42));
        // Frozen means frozen: the running step's clock stands still, however long a test takes.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(running.steps()[1].elapsed, Some(Duration::from_secs(40)));
        assert_eq!(running.elapsed(), Duration::from_secs(42));
        assert_eq!(
            running.lines()[..4],
            ["── rm", "$ sbx rm", "rm says hi", ""]
        );
        let failed = Job::frozen(
            "Creating",
            vec![step("create", StepState::Failed, Some(3))],
            None,
        );
        assert_eq!(
            failed.status(),
            Status::Failed("create failed (exit code 1)".into())
        );
        assert!(failed.is_finished());
        let done = Job::frozen(
            "Stopping",
            vec![step("stop", StepState::Done, Some(1))],
            None,
        );
        assert_eq!(done.status(), Status::Ok);
    }

    #[test]
    fn a_secret_on_stdin_is_in_neither_argv_nor_debug() {
        let step = Step::with_stdin(
            "sbx secret set-custom",
            vec!["sbx".into(), "secret".into(), "set-custom".into()],
            "sk-ant-oat01-real-token".into(),
        );
        assert!(!step.argv.iter().any(|a| a.contains("sk-ant")));
        assert!(!step.label.contains("sk-ant"));
        let debug = format!("{step:?}");
        assert!(!debug.contains("sk-ant"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
    }
}

//! Overlays: help, confirmations, rename, and the streaming job log.

use crate::input::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;

use super::Hits;
use crate::app::{App, Confirm, Hit, Modal, Prompt};
use crate::job::Status;
use crate::theme;

pub(super) fn render(frame: &mut Frame, app: &App, caps: theme::Caps) -> Hits {
    let Some(modal) = &app.modal else {
        return Hits::new();
    };
    match modal {
        Modal::Help => render_help(frame, app.help_scroll, caps),
        Modal::Confirm(confirm) => render_confirm(frame, confirm),
        Modal::Prompt(form) => render_prompt(frame, app, form),
        Modal::Running(job, pending) => render_job(frame, job, pending.fallback_hint.as_deref()),
    }
}

/// Centre a box of at most `w` × `h`, leaving a margin on small terminals.
fn centered(frame: &Frame, w: u16, h: u16) -> Rect {
    let area = frame.area();
    let w = w.min(area.width.saturating_sub(4)).max(20);
    let h = h.min(area.height.saturating_sub(2)).max(5);
    area.centered(Constraint::Length(w), Constraint::Length(h))
}

fn frame_block(title: &str, danger: bool) -> Block<'static> {
    Block::bordered()
        .border_type(BorderType::Plain)
        .border_style(if danger {
            theme::danger()
        } else {
            theme::border_focused()
        })
        .title(Span::styled(
            format!(" {title} "),
            if danger {
                theme::danger()
            } else {
                theme::title()
            },
        ))
        .padding(Padding::symmetric(2, 1))
}

/// The help sheet: each section's keys, and what they do. A row without a key is a note on the
/// row above it, drawn muted and indented under that row's text.
const HELP: &[(&str, &[(&str, &str)])] = &[
    (
        "Sandboxes",
        &[
            (
                "↑↓←→ / hjkl",
                "move between sandboxes (+ New sandbox closes this folder's)",
            ),
            ("g / G", "first / last row (Home / End)"),
            ("PgUp / PgDn", "a page of rows up / down"),
            (
                "/",
                "find a sandbox: type part of its name, agent, kit or folder",
            ),
            (
                "",
                "↑↓ step through the matches, ↵ stays there, esc goes back",
            ),
            ("↵", "start the selected sandbox — or create one, on + New"),
            ("x", "open a shell inside it (starts it if stopped)"),
            ("n", "create a new sandbox for this folder"),
            (
                "e",
                "kits, and which model its opus/sonnet/haiku aliases run",
            ),
            ("L", "LiteLLM sandboxes: fetch the gateway's model list now"),
            (
                "",
                "OpenCode picks it up at its next start, the pin list (e) at once",
            ),
            ("w", "which folder inside the workspace the agent opens in"),
            ("", "the whole workspace stays mounted — only the cwd moves"),
            (
                "A",
                "its Anthropic account, or a LiteLLM sandbox's connection",
            ),
            ("", "moving to another connection rebuilds the sandbox"),
            (
                "S",
                "every secret stored on this host: store, rotate, delete",
            ),
            (
                "",
                "LiteLLM connections: n new, ↵ edit URL and key, d delete",
            ),
            (
                "K",
                "the shared agent-skills store: import, add from a repo, update, remove",
            ),
            (
                "F",
                "host folders: the folders every sandbox mounts, with what each is for",
            ),
            ("T", "settings: sbx's own, for this host, such as telemetry"),
            (
                "i",
                "repo skills: this sandbox's picks from the configured skill repos",
            ),
            (
                "",
                "for every sandbox: K, then Repo skills for all sandboxes",
            ),
            ("r", "rename (label only — the sbx name never changes)"),
            ("R", "rebuild from its current kits"),
            ("s", "stop a running sandbox"),
            ("d / Del", "delete the sandbox"),
            ("P", "prune — remove every stopped sandbox on this host"),
            (
                "",
                "a running one is never a candidate; it names them first",
            ),
            ("a", "show / hide the sandboxes of other folders"),
        ],
    ),
    (
        "Network",
        &[
            ("p", "publish a host port into the sandbox"),
            ("u", "unpublish one again"),
            (
                "b",
                "keep it running after its sessions end, so a port keeps answering",
            ),
            ("", "sbx's detached mode — b again rebuilds it back"),
            ("c", "check whether a host is reachable from this sandbox"),
            (
                "y",
                "requests sandboxes are holding for approval: a allows, d dismisses",
            ),
            (
                "",
                "under balanced / deny-all a request no rule allows waits there",
            ),
        ],
    ),
    (
        "Connect",
        &[
            (
                "o",
                "connect VS Code, Cursor, JetBrains, Claude Desktop or ssh",
            ),
            ("", "every sandbox is an SSH host at <sbx name>.sbx"),
            (
                "",
                "an editor installs its server inside — needs the remote-dev kit",
            ),
        ],
    ),
    (
        "Forms (n, e, A, S, K, i, o, w, y)",
        &[
            ("↑↓ / tab", "move between rows"),
            ("←→", "move between the buttons the form ends with"),
            ("←→", "on w: out of / into a folder"),
            ("space", "tick a box, pick a radio"),
            ("↵", "activate the row under the cursor"),
            ("esc", "back, changing nothing"),
        ],
    ),
    (
        "Details",
        &[
            ("tab / ⇧tab", "next / previous tab"),
            ("1 … 7", "jump to a tab"),
            ("ctrl-↑↓", "scroll the details pane"),
            ("m", "maximize the details pane; esc restores it"),
        ],
    ),
    (
        "Everything else",
        &[
            ("mouse", "click a row, tab or footer hint; wheel scrolls"),
            (
                "M",
                "toggle mouse reporting (off = terminal text selection)",
            ),
            ("F5 / ctrl-r", "refresh now (auto-refreshes every 5 s)"),
            (
                "ctrl-z",
                "suspend to the shell; fg brings it back (not on Windows)",
            ),
            ("q / ctrl-c", "quit"),
        ],
    ),
];

/// The help sheet's widest box, its key column (room for `tab / shift-tab`, the widest key
/// without Unicode), and how far a note is indented under its row's text.
const HELP_WIDTH: u16 = 78;
const HELP_KEY_W: usize = 15;
const HELP_NOTE_INDENT: &str = "  ";

/// The keys are worded as the footer words them (`theme::key_name`): on a terminal without
/// Unicode, `enter` and `arrows` rather than the one-character stand-ins `theme::degrade` draws.
fn render_help(frame: &mut Frame, scroll: u16, caps: theme::Caps) -> Hits {
    // `room`: the box less its borders, `frame_block`'s padding and the key column.
    let width = HELP_WIDTH.min(frame.area().width.saturating_sub(4)).max(20);
    let room = (width as usize)
        .saturating_sub(2 + 4 + HELP_KEY_W + 3)
        .max(10);
    let mut lines: Vec<Line> = Vec::new();
    for (title, rows) in HELP {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            (*title).to_string(),
            theme::muted().add_modifier(Modifier::BOLD),
        )));
        for (key, what) in rows.iter() {
            let (indent, style) = match key.is_empty() {
                true => (HELP_NOTE_INDENT, theme::muted()),
                false => ("", Style::default()),
            };
            for (i, part) in super::wrap_text(what, room - indent.len())
                .into_iter()
                .enumerate()
            {
                let key = if i == 0 {
                    theme::key_name(key, caps)
                } else {
                    String::new()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{key:>HELP_KEY_W$}"), theme::key()),
                    Span::raw("   "),
                    Span::raw(indent),
                    Span::styled(part, style),
                ]));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Markers",
        theme::muted().add_modifier(Modifier::BOLD),
    )));
    for (marker, style, text) in [
        ("●", Style::default().fg(theme::RUNNING), "running"),
        ("○", Style::default().fg(theme::STOPPED), "stopped"),
        (
            "⟳",
            theme::warn(),
            "a rebuild is due: an update is ready, or a setup token is waiting",
        ),
        (
            "⚑",
            theme::warn(),
            "it is holding a network request for approval — y answers it",
        ),
        (
            "⚠",
            theme::danger(),
            "it cannot run: its token is missing, or it stopped responding",
        ),
        (
            "web",
            Style::default().fg(theme::kit_color("web")),
            "this sandbox has open egress to every host",
        ),
    ] {
        lines.push(Line::from(vec![
            Span::styled(format!("{marker:>HELP_KEY_W$}"), style),
            Span::raw("   "),
            Span::styled(text.to_string(), theme::muted()),
        ]));
    }

    let area = centered(frame, HELP_WIDTH, lines.len() as u16 + 4);
    frame.render_widget(Clear, area);
    let block = frame_block("Keys", false);
    let inner = block.inner(area);
    let total = lines.len() as u16;
    let scroll = scroll.min(total.saturating_sub(inner.height));
    let below = total.saturating_sub(scroll + inner.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)).block(block), area);
    let cut = match (scroll, below) {
        (_, 1..) => format!(" ↓ {below} more "),
        (1.., 0) => " ↑ top ".to_string(),
        (0, 0) => String::new(),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(cut, theme::muted())).right_aligned(),
        Rect {
            y: area.bottom().saturating_sub(1),
            height: 1,
            width: area.width.saturating_sub(2),
            ..area
        },
    );
    // The wheel scrolls; a click anywhere else in the box closes it.
    vec![(area, Hit::key(KeyCode::Esc)), (inner, Hit::ScrollHelp)]
}

fn render_confirm(frame: &mut Frame, confirm: &Confirm) -> Hits {
    // Wrap here, not with `Wrap`: the line count sizes the box and places the click targets.
    const WIDTH: u16 = 76;
    let text_width = (WIDTH - 2 - 4) as usize; // borders plus `frame_block`'s padding
    let mut lines: Vec<Line> = confirm
        .body
        .iter()
        .flat_map(|l| super::wrap_text(l, text_width))
        .map(|l| Line::from(Span::raw(l)))
        .collect();
    lines.push(Line::from(""));

    let yes_style = if confirm.yes {
        theme::danger().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        theme::danger()
    };
    let no_style = if confirm.yes {
        Style::default().fg(theme::ACCENT)
    } else {
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    };
    let yes_label = format!("  {}  ", confirm.confirm_label);
    let yes_width = yes_label.chars().count() as u16;
    let button_line = lines.len() as u16;
    lines.push(Line::from(vec![
        Span::styled(yes_label, yes_style),
        Span::raw("   "),
        Span::styled("  Cancel  ", no_style),
    ]));

    let area = centered(frame, WIDTH, lines.len() as u16 + 4);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(frame_block(&confirm.title, confirm.danger)),
        area,
    );

    // `frame_block` pads by two columns and one row inside the border.
    let (x, y) = (area.x + 3, area.y + 2 + button_line);
    if y >= area.bottom() {
        return Hits::new();
    }
    vec![
        (
            Rect {
                x,
                y,
                width: yes_width,
                height: 1,
            },
            Hit::key(KeyCode::Char('y')),
        ),
        (
            Rect {
                x: x + yes_width + 3,
                y,
                width: 10,
                height: 1,
            },
            Hit::key(KeyCode::Esc),
        ),
    ]
}

/// One line of text about the selected sandbox — a label, a port spec, a host to check — or a
/// secret's value, which is drawn as dots however it was typed.
fn render_prompt(frame: &mut Frame, app: &App, form: &Prompt) -> Hits {
    // A secret never appears on screen; the caret still walks its real characters.
    let shown: Vec<char> = match form.masked() {
        true => "•"
            .repeat(form.input.value.chars().count())
            .chars()
            .collect(),
        false => form.input.value.chars().collect(),
    };
    let before: String = shown[..form.input.cursor.min(shown.len())].iter().collect();
    let at = shown.get(form.input.cursor).copied().unwrap_or(' ');
    let after: String = shown
        .get(form.input.cursor + 1..)
        .map(|c| c.iter().collect())
        .unwrap_or_default();

    // The subject is a sandbox for most asks and a secret's name for the rest.
    let sandbox = app.sandboxes.iter().find(|s| s.name == form.subject);
    let subject = sandbox
        .map(|s| s.label.clone())
        .unwrap_or_else(|| form.subject.clone());

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("{subject}  "), Style::default().bold()),
            Span::styled(
                sandbox.map(|s| s.name.clone()).unwrap_or_default(),
                theme::muted(),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(" ▸ ", theme::muted()),
            Span::raw(before),
            Span::styled(
                at.to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
            Span::raw(after),
        ]),
        Line::from(""),
    ];
    lines.extend(
        form.help
            .iter()
            .flat_map(|l| super::wrap_text(l, 74))
            .map(|l| Line::from(Span::styled(l, theme::muted()))),
    );

    let area = centered(frame, 82, lines.len() as u16 + 4);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(frame_block(&form.title(), false)),
        area,
    );
    Hits::new()
}

/// A job in its frame: the log fills it, and the bottom border says where the job is, as a status
/// bar does. Under the log, the running step's note or, once the job failed, why and what to try.
fn render_job(frame: &mut Frame, job: &crate::job::Job, fallback_hint: Option<&str>) -> Hits {
    let area = centered(frame, 100, frame.area().height.saturating_sub(4).max(12));
    frame.render_widget(Clear, area);

    let status = job.status();
    let danger = matches!(status, Status::Failed(_));
    let block = frame_block(&job.title, danger);
    let inner = block.inner(area);

    // The strip sizes itself, as a note can wrap on a narrow modal; +1 is the blank row above it.
    let strip = job_strip(job.progress(), &status, fallback_hint);
    let strip_h = match strip.is_empty() {
        true => 0,
        false => (strip.iter())
            .map(|l| wrapped_rows(l, inner.width))
            .sum::<u16>()
            .saturating_add(1)
            .min(inner.height.saturating_sub(1)),
    };
    let [log_area, strip_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(strip_h)]).areas(inner);

    let rows = log_rows(&job.lines(), log_area.width);
    // Follow the tail while the job runs; a manual scroll pins the view.
    let last = rows.len().saturating_sub(log_area.height as usize);
    let scroll = match job.follow {
        true => last,
        false => (job.scroll as usize).min(last),
    };
    let hint = log_hint(scroll, last - scroll);
    let hint_w = hint.as_ref().map_or(0, Line::width);
    let room = (area.width as usize).saturating_sub(4 + hint_w);
    let mut block = block.title_bottom(job_border(job.progress(), &status, job.elapsed(), room));
    if let Some(hint) = hint {
        block = block.title_bottom(hint.right_aligned());
    }
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(rows).scroll((scroll.min(u16::MAX as usize) as u16, 0)),
        log_area,
    );
    let strip_area = Rect {
        y: strip_area.y.saturating_add(1),
        height: strip_area.height.saturating_sub(1),
        ..strip_area
    };
    frame.render_widget(Paragraph::new(strip).wrap(Wrap { trim: true }), strip_area);

    let mut hits = vec![(log_area, Hit::ScrollLog)];
    if job.is_finished() && strip_area.height > 0 {
        hits.push((strip_area, Hit::key(KeyCode::Enter)));
    }
    hits
}

/// The frames of the bottom border's spinner, one each 100 ms of the job's clock. Without
/// Unicode each draws as one of `| / - \` in turn ([`theme::ascii`]).
const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

/// The indent of the rest of a log line too long for the box.
const LOG_CONT: &str = "  ";

/// What the job's bottom border says, in at most `room` columns: while it runs a spinner, the
/// step it is on — of how many, when there are more — and the clock; once it ended, how.
fn job_border(
    progress: Option<crate::job::Progress>,
    status: &Status,
    elapsed: std::time::Duration,
    room: usize,
) -> Line<'static> {
    let clock = clock(elapsed);
    match status {
        Status::Running => {
            let frame = (elapsed.as_millis() / 100) as usize % SPINNER.len();
            let mut spans = vec![
                Span::raw(" "),
                Span::styled(SPINNER[frame], Style::default().fg(theme::ACCENT)),
                Span::raw(" "),
            ];
            let tail = Span::styled(format!(" · {clock} "), theme::muted());
            match progress {
                Some(p) => {
                    if p.total > 1 {
                        spans.push(Span::styled(
                            format!("{}/{} ", p.index, p.total),
                            theme::key(),
                        ));
                    }
                    let used = spans.iter().map(Span::width).sum::<usize>() + tail.width();
                    let label = super::clip(&p.label, room.saturating_sub(used));
                    spans.push(Span::raw(label));
                }
                None => spans.push(Span::styled("starting", theme::muted())),
            }
            spans.push(tail);
            Line::from(spans)
        }
        Status::Ok => Line::from(Span::styled(
            format!(" ✓ done · {clock} "),
            Style::default().fg(theme::RUNNING),
        )),
        Status::Failed(_) => {
            let at = match progress {
                Some(p) if p.total > 1 => format!(" at step {}/{}", p.index, p.total),
                _ => String::new(),
            };
            Line::from(Span::styled(
                format!(" ✗ failed{at} · {clock} "),
                theme::danger(),
            ))
        }
    }
}

/// A job's clock as a terminal writes one: minutes and seconds, `0:42`, `12:05`.
fn clock(elapsed: std::time::Duration) -> String {
    let s = elapsed.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

/// The log lines off screen, for the right of the bottom border: above the view, and below it
/// once a scroll left the tail. The keys are the footer's.
fn log_hint(above: usize, below: usize) -> Option<Line<'static>> {
    let mut spans = Vec::new();
    if above > 0 {
        spans.push(Span::styled(format!(" ↑ {above}"), theme::muted()));
    }
    if below > 0 {
        spans.push(Span::styled(format!(" ↓ {below} more"), theme::muted()));
    }
    if spans.is_empty() {
        return None;
    }
    spans.push(Span::raw(" "));
    Some(Line::from(spans))
}

/// The log as rows `width` columns wide: a line too long for the box goes on under itself,
/// [`LOG_CONT`] in, and each row keeps its line's look ([`log_style`]).
fn log_rows(lines: &[String], width: u16) -> Vec<Line<'static>> {
    let width = (width as usize).max(LOG_CONT.len() + 4);
    let mut rows = Vec::new();
    for line in lines {
        let style = log_style(line);
        let (head, mut rest) = log_row(line, width);
        rows.push(Line::from(Span::styled(head.to_string(), style)));
        while !rest.is_empty() {
            let (head, tail) = log_row(rest, width - LOG_CONT.len());
            rows.push(Line::from(Span::styled(format!("{LOG_CONT}{head}"), style)));
            rest = tail;
        }
    }
    rows
}

/// One row of a log line `width` columns wide, and the rest: up to the last space in the row's
/// final third when the line goes on, else as much as fits.
fn log_row(text: &str, width: usize) -> (&str, &str) {
    let (head, tail) = split_at_width(text, width);
    if tail.is_empty() || tail.starts_with(' ') {
        return (head, tail.trim_start_matches(' '));
    }
    let near = head.len() - head.len() / 3;
    match head.rfind(' ') {
        Some(at) if at >= near => (&text[..at], &text[at + 1..]),
        _ => (head, tail),
    }
}

/// How a log line looks: a step's rule (and sbx's own phase headers, which it writes the same
/// way) in the title's colour, a command muted, an error in red, the rest as printed.
fn log_style(line: &str) -> Style {
    if line.starts_with("── ") {
        theme::title()
    } else if line.starts_with("$ ") {
        theme::muted()
    } else if line.starts_with("error:") {
        Style::default().fg(theme::DANGER)
    } else {
        Style::default()
    }
}

/// `text` split where it fills `width` columns: what fits, and the rest. At least one character
/// goes in the first part, so a character wider than `width` still moves on.
fn split_at_width(text: &str, width: usize) -> (&str, &str) {
    let mut used = 0;
    for (i, c) in text.char_indices() {
        let w = match c.is_ascii() {
            true => usize::from(!c.is_ascii_control()),
            false => theme::unicode_width(c),
        };
        if used + w > width && i > 0 {
            return text.split_at(i);
        }
        used += w;
    }
    (text, "")
}

/// How many rows a line takes once `Wrap { trim: true }` has had it: greedy at word boundaries,
/// with an over-long word taking further rows to itself.
fn wrapped_rows(line: &Line, width: u16) -> u16 {
    let width = width.max(1) as usize;
    let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
    let (mut rows, mut used) = (1u16, 0usize);
    for word in text.split_whitespace() {
        let w = word.chars().count();
        if used == 0 {
            used = w;
        } else if used + 1 + w <= width {
            used += 1 + w;
        } else {
            rows = rows.saturating_add(1);
            used = w;
        }
        while used > width {
            rows = rows.saturating_add(1);
            used -= width;
        }
    }
    rows
}

/// The strip under a job's log: the running step's note ([`crate::job::Step::note`]) while it
/// runs; once it failed, why, and what to try next.
fn job_strip(
    progress: Option<crate::job::Progress>,
    status: &Status,
    fallback_hint: Option<&str>,
) -> Vec<Line<'static>> {
    match status {
        Status::Running => (progress.and_then(|p| p.note))
            .map(|note| vec![Line::from(Span::styled(note, theme::muted()))])
            .unwrap_or_default(),
        Status::Ok => Vec::new(),
        Status::Failed(msg) => {
            let mut lines = vec![Line::from(Span::styled(msg.clone(), theme::danger()))];
            if let Some(hint) = fallback_hint {
                lines.push(Line::from(Span::styled(hint.to_string(), theme::muted())));
            }
            lines
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Progress;

    fn step(index: usize, total: usize, note: Option<&str>) -> Progress {
        Progress {
            index,
            total,
            label: "sbx create claude-litellm-proj".into(),
            note: note.map(str::to_string),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_running_jobs_border_names_its_step_and_its_clock() {
        let secs = std::time::Duration::from_secs;
        let border = |p, status: &Status, t| line_text(&job_border(p, status, t, 100));
        let two = border(Some(step(2, 3, None)), &Status::Running, secs(42));
        assert!(
            SPINNER.iter().any(|f| two.starts_with(&format!(" {f} "))),
            "{two}"
        );
        assert!(
            two.ends_with(" 2/3 sbx create claude-litellm-proj · 0:42 "),
            "{two}"
        );
        // A single-step job has no "1/1" worth showing.
        let solo = border(Some(step(1, 1, None)), &Status::Running, secs(5));
        assert!(
            solo.ends_with(" sbx create claude-litellm-proj · 0:05 "),
            "{solo}"
        );
        assert!(!solo.contains("1/1"), "{solo}");
        // Before the first step starts there is nothing to name, and nothing is invented.
        let starting = border(None, &Status::Running, secs(0));
        assert!(starting.ends_with(" starting · 0:00 "), "{starting}");
        // Once it ended: how, and after how long.
        let failed = Status::Failed("sbx create box failed (exit code 1)".into());
        let at = border(Some(step(2, 3, None)), &failed, secs(75));
        assert_eq!(at, " ✗ failed at step 2/3 · 1:15 ");
        assert_eq!(
            border(Some(step(3, 3, None)), &Status::Ok, secs(605)),
            " ✓ done · 10:05 "
        );
    }

    #[test]
    fn the_spinner_turns_and_turns_in_ascii_too() {
        let frame = |ms: u64| {
            let line = job_border(
                Some(step(1, 1, None)),
                &Status::Running,
                std::time::Duration::from_millis(ms),
                100,
            );
            line.spans[1].content.to_string()
        };
        let frames: Vec<String> = (0..8).map(|i| frame(i * 100)).collect();
        assert_eq!(frames, SPINNER);
        assert_eq!(frame(800), SPINNER[0], "round again");
        let ascii: Vec<&str> = SPINNER.iter().map(|f| theme::ascii(f).unwrap()).collect();
        assert_eq!(ascii, ["|", "/", "-", "\\", "|", "/", "-", "\\"]);
    }

    #[test]
    fn a_long_step_is_cut_on_the_border_and_the_clock_stays() {
        let line = job_border(
            Some(step(2, 3, None)),
            &Status::Running,
            std::time::Duration::from_secs(9),
            30,
        );
        let text = line_text(&line);
        assert!(line.width() <= 30, "{text}");
        assert!(
            text.contains("2/3 sbx create") && text.contains('…'),
            "{text}"
        );
        assert!(text.ends_with(" · 0:09 "), "{text}");
    }

    #[test]
    fn the_strip_says_the_steps_note_or_why_the_job_failed() {
        let strip = |p, status: &Status, hint| -> Vec<String> {
            job_strip(p, status, hint).iter().map(line_text).collect()
        };
        let noted = strip(
            Some(step(2, 2, Some("pulling the image"))),
            &Status::Running,
            None,
        );
        assert_eq!(noted, ["pulling the image"]);
        assert!(strip(Some(step(1, 1, None)), &Status::Running, None).is_empty());
        assert!(strip(None, &Status::Ok, None).is_empty());
        let failed = Status::Failed("sbx create box failed (exit code 1)".into());
        let why = strip(
            Some(step(2, 2, None)),
            &failed,
            Some("Rebuild once it is fixed."),
        );
        assert_eq!(
            why,
            [
                "sbx create box failed (exit code 1)",
                "Rebuild once it is fixed."
            ]
        );
    }

    #[test]
    fn a_long_log_line_goes_on_under_itself() {
        let command =
            "$ sbx create --name box --kit /Users/u/kits/jdk25 --kit /Users/u/kits/rtk /proj";
        let rows = log_rows(&[command.to_string()], 30);
        let text: Vec<String> = rows.iter().map(line_text).collect();
        assert!(rows.len() > 2, "{text:?}");
        for row in &text[1..] {
            assert!(
                row.starts_with(LOG_CONT) && !row[LOG_CONT.len()..].starts_with(' '),
                "{text:?}"
            );
        }
        assert!(rows.iter().all(|r| r.width() <= 30), "{text:?}");
        let joined = text.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
        assert_eq!(joined, command, "nothing is lost");
        assert!(
            rows.iter().all(|r| r.spans[0].style == theme::muted()),
            "a command stays muted"
        );
        // Words stay whole where a space is near the end of the row.
        let words = log_rows(&["aaaa bbbb cccc dddd".to_string()], 12);
        assert_eq!(
            words.iter().map(line_text).collect::<Vec<_>>(),
            ["aaaa bbbb", "  cccc dddd"]
        );
        // A hash has no space: it breaks where the row is full.
        let hash = log_rows(&["sha256:0123456789abcdef".to_string()], 12);
        let hash: Vec<String> = hash.iter().map(line_text).collect();
        assert_eq!(hash, ["sha256:01234", "  56789abcde", "  f"]);
    }

    #[test]
    fn each_kind_of_log_line_has_its_look() {
        assert_eq!(log_style("── sbx create box"), theme::title());
        assert_eq!(log_style("── PREPARE IMAGE"), theme::title());
        assert_eq!(log_style("$ sbx rm --force box"), theme::muted());
        assert_eq!(
            log_style("error: could not start `sbx`"),
            Style::default().fg(theme::DANGER)
        );
        assert_eq!(log_style("Resolving kits"), Style::default());
    }

    fn frozen(state: crate::job::StepState) -> crate::job::Job {
        use crate::job::{StepState, StepView};
        let create = "sbx create --name claude-litellm-proj --kit /Users/u/aisbx/kits/jdk25 \
             --kit /Users/u/aisbx/kits/rtk /Users/u/aisbx/kits/claude-litellm /proj-at-the-end";
        let steps = vec![
            StepView {
                label: "sbx secret set-custom".into(),
                command: "sbx secret set-custom --host gw.example.org".into(),
                state: StepState::Done,
                output: vec!["Stored".into()],
                elapsed: Some(std::time::Duration::from_millis(300)),
            },
            StepView {
                label: "sbx create claude-litellm-proj".into(),
                command: create.into(),
                state,
                output: vec!["── PREPARE IMAGE".into(), "Resolving kits".into()],
                elapsed: Some(std::time::Duration::from_secs(40)),
            },
            StepView {
                label: "write the label".into(),
                command: "sbx exec".into(),
                state: StepState::Waiting,
                output: Vec::new(),
                elapsed: None,
            },
        ];
        crate::job::Job::frozen(
            "Creating claude-litellm-proj",
            steps,
            Some("pulling the image"),
        )
    }

    fn draw_job(job: &crate::job::Job, hint: Option<&str>) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_job(frame, job, hint);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_running_create_shows_its_whole_command_and_its_step_on_the_border() {
        let rows = draw_job(&frozen(crate::job::StepState::Running), None);
        let screen = rows.join("\n");
        assert!(screen.contains("/proj-at-the-end"), "{screen}");
        assert!(screen.contains("pulling the image"), "{screen}");
        let border = rows
            .iter()
            .rfind(|r| r.contains('└'))
            .expect("a bottom border");
        assert!(
            border.contains(" 2/3 sbx create claude-litellm-proj · 0:4"),
            "{border}"
        );
        assert!(
            !screen.contains("running 2/3"),
            "the old status line is gone"
        );
    }

    #[test]
    fn a_failed_create_says_which_step_and_how_it_ended() {
        let rows = draw_job(
            &frozen(crate::job::StepState::Failed),
            Some("Rebuild once fixed."),
        );
        let screen = rows.join("\n");
        assert!(
            screen.contains("sbx create claude-litellm-proj failed (exit code 1)"),
            "{screen}"
        );
        assert!(screen.contains("Rebuild once fixed."), "{screen}");
        let border = rows
            .iter()
            .rfind(|r| r.contains('└'))
            .expect("a bottom border");
        assert!(border.contains("✗ failed at step 2/3 · 0:4"), "{border}");
        assert!(
            !screen.contains("pulling the image"),
            "a failed job has no note"
        );
    }

    #[test]
    fn the_help_sheet_cuts_no_description() {
        let whole = |screen: &str, word: &str| {
            screen.match_indices(word).any(|(at, _)| {
                let before = screen[..at].chars().next_back();
                let after = screen[at + word.len()..].chars().next();
                [before, after]
                    .iter()
                    .all(|c| c.is_none_or(|c| !c.is_alphanumeric()))
            })
        };
        for width in [110u16, 60, 46] {
            let backend = ratatui::backend::TestBackend::new(width, 240);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render_help(frame, 0, theme::Caps::FULL);
                })
                .unwrap();
            let screen = terminal.backend().to_string();
            for (_, rows) in HELP {
                for (_, what) in rows.iter() {
                    for word in what.split_whitespace() {
                        assert!(whole(&screen, word), "{width}: {word:?} of {what:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_help_sheet_names_every_key_that_moves_the_cursor() {
        let keys: Vec<&str> = HELP[0].1.iter().map(|(key, _)| *key).collect();
        let named = |k: &str| {
            keys.iter()
                .any(|key| key.split(" / ").any(|part| part == k))
        };
        for key in ["↑↓←→", "hjkl", "g", "G", "PgUp", "PgDn", "/"] {
            assert!(named(key), "{key} in {keys:?}");
        }
        let first_last = HELP[0].1.iter().find(|(key, _)| *key == "g / G").unwrap();
        assert!(first_last.1.contains("Home / End"), "{first_last:?}");
    }

    #[test]
    fn without_unicode_the_help_sheet_words_its_keys() {
        let ascii = theme::Caps {
            color: false,
            unicode: false,
        };
        for caps in [theme::Caps::FULL, ascii] {
            for (_, rows) in HELP {
                for (key, _) in rows.iter() {
                    let name = theme::key_name(key, caps);
                    assert!(name.chars().count() <= HELP_KEY_W, "{name:?} is too wide");
                }
            }
        }
        let backend = ratatui::backend::TestBackend::new(110, 240);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help(frame, 0, ascii);
                theme::degrade(frame.buffer_mut(), ascii);
            })
            .unwrap();
        let screen = terminal.backend().to_string();
        for worded in [
            "arrows / hjkl",
            "enter",
            "tab / shift-tab",
            "ctrl-up/down",
            "1 ... 7",
        ] {
            assert!(screen.contains(worded), "{worded:?} on the sheet");
        }
        assert!(!screen.contains("?tab"), "⇧ drawn as its stand-in");
    }

    #[test]
    fn a_help_note_is_muted_and_indented_under_its_row() {
        let backend = ratatui::backend::TestBackend::new(60, 240);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render_help(frame, 0, theme::Caps::FULL);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| (0..60).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        // The line `text` is on, where it starts, and the colour of its first cell.
        let at = |text: &str| {
            let (y, row) = (rows.iter().enumerate())
                .find(|(_, r)| r.contains(text))
                .unwrap_or_else(|| panic!("{text:?} is on the sheet"));
            let x = row[..row.find(text).unwrap()].chars().count() as u16;
            (y, x, buffer[(x, y as u16)].fg)
        };
        let (y, row_x, row_fg) = at("every secret stored");
        // The line under it is the rest of the row's text, wrapped at this width.
        let wrap_x = (row_x..60)
            .find(|x| !buffer[(*x, y as u16 + 1)].symbol().trim().is_empty())
            .expect("the row wraps at 60 columns");
        let wrap_fg = buffer[(wrap_x, y as u16 + 1)].fg;
        let (_, note_x, note_fg) = at("LiteLLM connections:");
        assert_eq!(
            (wrap_x, wrap_fg),
            (row_x, row_fg),
            "a wrapped row keeps its column"
        );
        assert_eq!(note_x, row_x + HELP_NOTE_INDENT.len() as u16);
        assert_eq!(note_fg, theme::MUTED);
        assert_ne!(row_fg, theme::MUTED);
    }

    #[test]
    fn a_wrapped_line_is_measured_as_the_rows_it_takes() {
        let line = |s: &str| Line::from(Span::raw(s.to_string()));
        assert_eq!(wrapped_rows(&line("short"), 20), 1);
        assert_eq!(
            wrapped_rows(&line(""), 20),
            1,
            "an empty line is still a row"
        );
        // At width 10, greedily on spaces: "one two" / "three four" / "five".
        assert_eq!(wrapped_rows(&line("one two three four five"), 10), 3);
        // A word longer than the line takes rows of its own rather than counting as one.
        assert_eq!(wrapped_rows(&line("aaaaaaaaaaaaaaaaaaaaaaaa"), 10), 3);
    }
}

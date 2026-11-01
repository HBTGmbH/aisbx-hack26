//! Rendering. Every screen is a function of `&App` plus the click targets it registers.
//! A renderer returns its [`Hits`], and [`draw`] hands them to the app afterwards.

mod detail;
mod form;
mod modal;
mod roster;

use crate::input::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;

use crate::actions::Action;
use crate::app::{App, CreateRow, Hit, Modal, Screen};
use crate::create::{CreateStep, WizardRow};
use crate::model;
use crate::theme;

/// Click targets a renderer produced, in painting order.
pub(crate) type Hits = Vec<(Rect, Hit)>;

pub fn draw(frame: &mut Frame, app: &mut App) {
    draw_in(frame, app, theme::caps());
}

/// [`draw`] for a terminal that can show `caps`: drawn as ever, then taken down to it cell by cell
/// ([`theme::degrade`]), so a terminal without colour or Unicode gets the same layout.
pub(crate) fn draw_in(frame: &mut Frame, app: &mut App, caps: theme::Caps) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    app.clear_hits();
    let mut hits = match app.screen {
        Screen::List => render_list(frame, app, body),
        Screen::Create(_) => form::render_create(frame, app, body),
        Screen::Kits(_) => form::render_kits(frame, app, body),
        Screen::AuthEdit(_) => form::render_auth(frame, app, body),
        Screen::Secrets(_) => form::render_secrets(frame, app, body),
        Screen::Skills(_) => form::render_skills(frame, app, body),
        Screen::RepoSkills(_) => form::render_repo_skills(frame, app, body),
        Screen::Connect(_) => form::render_connect(frame, app, body),
        Screen::StartDir(_) => form::render_start_dir(frame, app, body),
        Screen::Approvals(_) => form::render_approvals(frame, app, body),
        Screen::Connection(_) => form::render_connection(frame, app, body),
        Screen::ConnSwitch(_) => form::render_conn_switch(frame, app, body),
        Screen::Folders(_) => form::render_folders(frame, app, body),
    };
    hits.extend(render_header(frame, app, header));
    let footer_hits = render_footer(frame, app, footer, caps);

    // While a modal is up it owns every click but the footer's, which names the modal's keys.
    let modal_hits = modal::render(frame, app, caps);
    if app.modal.is_some() {
        hits = modal_hits;
    } else {
        hits.extend(modal_hits);
    }
    hits.extend(footer_hits);
    for (rect, hit) in hits {
        app.hit(crate::keys::rect(rect), hit);
    }
    theme::degrade(frame.buffer_mut(), caps);
}

fn render_header(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let mut hits = Hits::new();
    let mut left = vec![
        Span::styled(" aisbx ", theme::title().add_modifier(Modifier::REVERSED)),
        Span::raw(" "),
        Span::styled(model::tilde(&app.cwd), Style::default().bold()),
    ];
    let waiting = app.approvals.len();
    let badge = match waiting {
        0 => String::new(),
        1 => "⚑ 1 approval waiting  ".to_string(),
        n => format!("⚑ {n} approvals waiting  "),
    };
    // The mark keeps its two cells while no refresh runs, so the badge never shifts.
    let refreshing = if app.loading { "↻ " } else { "  " };
    let tail = format!(
        "{refreshing}{}sbx {}  ",
        if app.mouse { "" } else { "mouse off  " },
        app.sbx_version
    );
    let right = Line::from(vec![
        Span::styled(badge.clone(), theme::warn().add_modifier(Modifier::BOLD)),
        Span::styled(refreshing, Style::default().fg(theme::ACCENT)),
        Span::styled(if app.mouse { "" } else { "mouse off  " }, theme::muted()),
        Span::styled(format!("sbx {}  ", app.sbx_version), theme::muted()),
    ])
    .right_aligned();
    let summary = host_summary(app);
    let used = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
    if used(&left) + used(&summary) + used(&right.spans) < area.width as usize {
        left.extend(summary);
    }
    let left = Line::from(left);

    frame.render_widget(Paragraph::new(left), area);
    frame.render_widget(Paragraph::new(right), area);
    // The badge is a button, like every footer hint: a click opens what it announces.
    if waiting > 0 {
        let used = (badge.chars().count() + tail.chars().count()) as u16;
        let x = area.right().saturating_sub(used);
        hits.push((
            Rect {
                x,
                y: area.y,
                width: (badge.chars().count() as u16).min(area.right().saturating_sub(x)),
                height: 1,
            },
            Hit::Press(KeyCode::Char('y'), KeyModifiers::NONE),
        ));
    }
    hits
}

/// `● 2 of 4 running` for the status line — the dot green while any runs, a grey ring while none
/// does — or nothing on a host without sandboxes.
fn host_summary(app: &App) -> Vec<Span<'static>> {
    let Some((running, all)) = app.host_running() else {
        return Vec::new();
    };
    let dot = match running {
        0 => Span::styled("○", theme::muted()),
        _ => Span::styled("●", Style::default().fg(theme::RUNNING)),
    };
    vec![
        Span::raw("   "),
        dot,
        Span::styled(format!(" {running} of {all} running"), theme::muted()),
    ]
}

fn render_footer(frame: &mut Frame, app: &App, area: Rect, caps: theme::Caps) -> Hits {
    // A search owns the line, before the status, which would hide what is typed.
    if app.find.is_some() && app.modal.is_none() && matches!(app.screen, Screen::List) {
        return render_find(frame, app, area, caps);
    }
    if let Some((msg, is_error, _)) = &app.status {
        let style = if *is_error {
            theme::danger()
        } else {
            Style::default().fg(theme::RUNNING)
        };
        let prefix = if *is_error { " ✗ " } else { " ✓ " };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix, style),
                Span::styled(msg.clone(), style),
            ]))
            .wrap(Wrap { trim: true }),
            area,
        );
        return Hits::new();
    }

    // Every hint is also a button: a click sends the key it names.
    if let Some(modal) = &app.modal {
        let hints = fit_hints(modal_hints(modal), area.width, caps, &[]);
        return draw_hints(frame, area, &hints, caps);
    }
    let on_new = matches!(app.screen, Screen::List) && app.on_new_card();
    let others = match (app.can(Action::ToggleOther), app.show_other) {
        (Err(_), _) => None,
        (Ok(()), true) => Some(("a", "hide others", KeyCode::Char('a'))),
        (Ok(()), false) => Some(("a", "show others", KeyCode::Char('a'))),
    };
    let hints: Vec<(&str, &str, KeyCode)> = match app.screen {
        Screen::List if on_new => {
            let mut hints = vec![("↵", "create", KeyCode::Enter)];
            // First after the primary key: an agent is blocked on the answer.
            if !app.approvals.is_empty() {
                hints.push(("y", "approvals", KeyCode::Char('y')));
            }
            hints.extend([
                ("S", "secrets", KeyCode::Char('S')),
                ("K", "skills", KeyCode::Char('K')),
            ]);
            if app.can_prune() {
                hints.push(("P", "prune", KeyCode::Char('P')));
            }
            hints.extend(others);
            hints.extend([
                ("/", "find", KeyCode::Char('/')),
                ("?", "help", KeyCode::Char('?')),
                ("q", "quit", KeyCode::Char('q')),
            ]);
            fit_hints(hints, area.width, caps, LIST_DROP)
        }
        Screen::List => {
            let mut hints = vec![("↵", "start", KeyCode::Enter)];
            if !app.approvals.is_empty() {
                hints.push(("y", "approvals", KeyCode::Char('y')));
            }
            hints.extend([
                ("x", "shell", KeyCode::Char('x')),
                ("n", "new", KeyCode::Char('n')),
                ("e", "kits", KeyCode::Char('e')),
            ]);
            if app.can_connect() {
                hints.push(("o", "connect", KeyCode::Char('o')));
            }
            if app.can_pick_start_dir() {
                hints.push(("w", "start folder", KeyCode::Char('w')));
            }
            if app.can_edit_auth() {
                let label = match app.selected().is_some_and(|sb| app.on_litellm_backend(sb)) {
                    true => "connection",
                    false => "account",
                };
                hints.push(("A", label, KeyCode::Char('A')));
            }
            hints.extend([
                ("i", "repo skills", KeyCode::Char('i')),
                ("S", "secrets", KeyCode::Char('S')),
                ("K", "skills", KeyCode::Char('K')),
            ]);
            if app.can_prune() {
                hints.push(("P", "prune", KeyCode::Char('P')));
            }
            hints.push(("p", "port", KeyCode::Char('p')));
            // Next to the ports, which are what it is for.
            if app.can_keep_running() {
                hints.push((
                    "b",
                    match app.selected().is_some_and(|sb| sb.keep_running) {
                        true => "auto-stop",
                        false => "keep running",
                    },
                    KeyCode::Char('b'),
                ));
            }
            hints.extend([
                ("r", "rename", KeyCode::Char('r')),
                ("R", "rebuild", KeyCode::Char('R')),
                ("s", "stop", KeyCode::Char('s')),
                ("d", "delete", KeyCode::Char('d')),
            ]);
            hints.extend(others);
            hints.extend([
                ("/", "find", KeyCode::Char('/')),
                ("?", "help", KeyCode::Char('?')),
                ("q", "quit", KeyCode::Char('q')),
            ]);
            fit_hints(hints, area.width, caps, LIST_DROP)
        }
        // The wizard: the cursor row's keys first, then the keys that work from any row.
        Screen::Create(ref form) => {
            let rows = form.wizard_rows(&app.catalog);
            let row = rows.get(form.wizard_focus().min(rows.len().saturating_sub(1)));
            let typing = matches!(row, Some(WizardRow::Field(f)) if f.is_text());
            let mut hints: Vec<(&str, &str, KeyCode)> = match row {
                _ if app.on_form_buttons() => vec![
                    ("←→", "pick", KeyCode::Left),
                    ("↵", "activate", KeyCode::Enter),
                ],
                // The skills mode and the LiteLLM connection: one of several named values.
                _ if app.on_create_choice_row() => vec![
                    ("↑↓", "move", KeyCode::Down),
                    ("←→", "pick", KeyCode::Right),
                ],
                _ if typing => vec![
                    ("↑↓", "move", KeyCode::Down),
                    ("↵", "next row", KeyCode::Enter),
                ],
                Some(WizardRow::Field(
                    CreateRow::Addon(_)
                    | CreateRow::Clone
                    | CreateRow::GitGuard
                    | CreateRow::Mcp(_),
                )) => vec![
                    ("↑↓", "move", KeyCode::Down),
                    ("space", "toggle", KeyCode::Char(' ')),
                ],
                // A config file opens in an editor; Delete drops the user's copy.
                Some(WizardRow::Field(CreateRow::ConfigFile(_))) => vec![
                    ("↑↓", "move", KeyCode::Down),
                    ("↵", "edit", KeyCode::Enter),
                    ("del", "reset", KeyCode::Delete),
                ],
                // The step's own radios: ↵ picks and goes on.
                Some(
                    WizardRow::Provider(_)
                    | WizardRow::Agent(_)
                    | WizardRow::Connection(_)
                    | WizardRow::Field(CreateRow::TokenPick(_)),
                ) => vec![
                    ("↑↓", "move", KeyCode::Down),
                    ("↵", "pick & next", KeyCode::Enter),
                ],
                _ => vec![("↑↓", "move", KeyCode::Down), ("↵", "pick", KeyCode::Enter)],
            };
            match typing {
                // A text row takes the line-editing keys of a shell prompt; this one clears it.
                true => hints.push(("ctrl-u", "clear", KeyCode::Char('u'))),
                // Named, not pressed: a click on it has no one step to go to.
                false => hints.push(("1-7", "step", KeyCode::Null)),
            }
            if form.wizard_step() != CreateStep::Review {
                hints.push(("pgdn", "next", KeyCode::PageDown));
            }
            hints.extend([
                ("ctrl-s", "create & start", KeyCode::Char('s')),
                ("esc", "back", KeyCode::Esc),
            ]);
            // PgDn first, since ↵ and the trail go on as well.
            fit_hints(hints, area.width, caps, &["pgdn", "ctrl-u", "1-7", "del"])
        }
        Screen::Kits(_)
        | Screen::AuthEdit(_)
        | Screen::StartDir(_)
        | Screen::Connection(_)
        | Screen::ConnSwitch(_)
        | Screen::Folders(_)
            if app.on_form_buttons() =>
        {
            vec![
                ("←→", "pick", KeyCode::Left),
                ("↵", "activate", KeyCode::Enter),
                ("↑↓/tab", "move", KeyCode::Down),
                ("esc", "back", KeyCode::Esc),
            ]
        }
        // On a connection row Enter opens its dialog, which edits more than a value.
        Screen::Secrets(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("n", "new connection", KeyCode::Char('n')),
            match app.on_connection_row() {
                true => ("↵", "edit", KeyCode::Enter),
                false => ("↵", "store / replace", KeyCode::Enter),
            },
            ("d", "delete", KeyCode::Char('d')),
            ("esc", "back", KeyCode::Esc),
        ],
        // ←→ picks the connection on `A`'s first row, and moves the caret in a text field.
        Screen::Connection(_) | Screen::ConnSwitch(_) => vec![
            ("↑↓/tab", "move", KeyCode::Down),
            ("↵", "next / pick", KeyCode::Enter),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::Folders(_) => vec![
            ("↑↓/tab", "move", KeyCode::Down),
            ("↵", "edit / next", KeyCode::Enter),
            ("n", "new", KeyCode::Char('n')),
            ("d", "delete", KeyCode::Char('d')),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::Skills(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("↵", "activate", KeyCode::Enter),
            ("i", "import", KeyCode::Char('i')),
            ("a", "add repo", KeyCode::Char('a')),
            ("u", "update", KeyCode::Char('u')),
            ("d", "remove", KeyCode::Char('d')),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::RepoSkills(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("space", "tick", KeyCode::Char(' ')),
            ("↵", "activate", KeyCode::Enter),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::Connect(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("↵", "connect", KeyCode::Enter),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::Approvals(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("a/↵", "allow", KeyCode::Char('a')),
            ("d", "dismiss", KeyCode::Char('d')),
            ("esc", "back", KeyCode::Esc),
        ],
        // Left/right walks the tree here, not the button line, so it is named before Enter.
        Screen::StartDir(_) => vec![
            ("←→", "out / in", KeyCode::Right),
            ("↑↓", "move", KeyCode::Down),
            ("↵", "open", KeyCode::Enter),
            ("esc", "back", KeyCode::Esc),
        ],
        Screen::Kits(_) | Screen::AuthEdit(_) => vec![
            ("↑↓", "move", KeyCode::Down),
            ("space", "toggle", KeyCode::Char(' ')),
            ("↵", "activate", KeyCode::Enter),
            ("esc", "back", KeyCode::Esc),
        ],
    };

    draw_hints(frame, area, &hints, caps)
}

/// The `/` prompt on the footer line: the query with its caret, where the cursor is among the
/// matches, and the keys a search takes — each a button, as every hint is.
fn render_find(frame: &mut Frame, app: &App, area: Rect, caps: theme::Caps) -> Hits {
    let query = app.find.as_ref().map_or("", |f| f.query.as_str());
    let mut spans = vec![
        Span::raw(" "),
        Span::styled("/", theme::key()),
        Span::styled(
            query.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        // The caret: a reversed cell, as every text field of the dashboard draws one.
        Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw("  "),
    ];
    spans.push(match (query.is_empty(), app.find_position()) {
        (true, _) => Span::styled(FIND_PROMPT, theme::muted()),
        (false, (_, 0)) => Span::styled("no match", theme::warn()),
        (false, (Some(at), n)) => Span::styled(format!("{at} of {n}"), theme::muted()),
        (false, (None, n)) => Span::styled(
            format!("{n} match{}", if n == 1 { "" } else { "es" }),
            theme::muted(),
        ),
    });
    let prompt = Line::from(spans);
    let used = (prompt.width() as u16 + 2).min(area.width);
    frame.render_widget(Paragraph::new(prompt), area);
    let rest = Rect {
        x: area.x + used,
        width: area.width - used,
        ..area
    };
    let hints = vec![
        ("↑↓", "matches", KeyCode::Down),
        ("↵", "keep", KeyCode::Enter),
        ("esc", "back", KeyCode::Esc),
    ];
    let hints = fit_hints(hints, rest.width, caps, &["↑↓", "↵", "esc"]);
    draw_hints(frame, rest, &hints, caps)
}

/// The `/` prompt's line while nothing is typed yet: what a search reads.
pub(crate) const FIND_PROMPT: &str = "type part of a name, agent, kit or folder";

/// What the open modal takes, as footer hints: the keys it answers, in the order they matter.
fn modal_hints(modal: &Modal) -> Vec<(&'static str, &'static str, KeyCode)> {
    match modal {
        Modal::Help => vec![
            ("↑↓", "scroll", KeyCode::Down),
            ("esc", "close", KeyCode::Esc),
        ],
        Modal::Confirm(_) => vec![
            ("←→/tab", "choose", KeyCode::Tab),
            ("↵", "answer", KeyCode::Enter),
            ("y", "confirm", KeyCode::Char('y')),
            ("esc", "cancel", KeyCode::Esc),
        ],
        // ↵ is named by what answering does — `rename`, `publish`, `store` — as the GUI's button.
        Modal::Prompt(prompt) => vec![
            ("↵", prompt.verb(), KeyCode::Enter),
            ("esc", "cancel", KeyCode::Esc),
        ],
        Modal::Running(job, _) if !job.is_finished() => vec![
            ("↑↓", "scroll", KeyCode::Down),
            ("end", "follow", KeyCode::End),
        ],
        Modal::Running(job, pending) => {
            let failed = matches!(job.status(), crate::job::Status::Failed(_));
            let mut hints = Vec::new();
            if failed && pending.fallback.is_some() {
                hints.push(("r", "rebuild instead", KeyCode::Char('r')));
            }
            hints.extend([
                ("↵/esc", "close", KeyCode::Enter),
                ("↑↓", "scroll", KeyCode::Down),
            ]);
            hints
        }
    }
}

/// The footer's line of hints, each a button for its key.
fn draw_hints(
    frame: &mut Frame,
    area: Rect,
    hints: &[(&str, &str, KeyCode)],
    caps: theme::Caps,
) -> Hits {
    let mut hits = Hits::new();
    let mut spans = vec![Span::raw(" ")];
    let mut x = area.x + 1;
    for (key, label, code) in hints {
        let name = theme::key_name(key, caps);
        let width = (name.chars().count() + label.chars().count() + 1) as u16;
        spans.push(Span::styled(name, theme::key()));
        spans.push(Span::raw(" "));
        spans.push(Span::styled((*label).to_string(), theme::muted()));
        spans.push(Span::styled("  ", theme::muted()));
        if x < area.right() {
            hits.push((
                Rect {
                    x,
                    y: area.y,
                    width: width.min(area.right() - x),
                    height: 1,
                },
                // A `ctrl-` hint sends its key with Control held, as typing it does.
                Hit::Press(
                    *code,
                    match key.starts_with("ctrl-") {
                        true => KeyModifiers::CONTROL,
                        false => KeyModifiers::NONE,
                    },
                ),
            ));
        }
        x = x.saturating_add(width + 2);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    hits
}

/// The list's hints in the order they give way on a narrow footer: host-wide and rare keys first,
/// the session keys last. `↵`, `y` and `?` are not in it, so they always show.
const LIST_DROP: &[&str] = &[
    "q", "a", "/", "r", "P", "K", "S", "i", "b", "p", "w", "A", "o", "R", "e", "d", "s", "n", "x",
];

/// `hints` without whole hints, taken in `drop` order, until the footer's line fits `width`: a
/// hint cut at the terminal's edge names a key without saying what it does.
fn fit_hints<'a>(
    mut hints: Vec<(&'a str, &'a str, KeyCode)>,
    width: u16,
    caps: theme::Caps,
    drop: &[&str],
) -> Vec<(&'a str, &'a str, KeyCode)> {
    // A space before the first hint and two after each but the last, as `render_footer` draws.
    let used = |hints: &[(&str, &str, KeyCode)]| {
        let drawn: usize = hints
            .iter()
            .map(|(key, label, _)| {
                theme::key_name(key, caps).chars().count() + 1 + label.chars().count() + 2
            })
            .sum();
        (1 + drawn).saturating_sub(2)
    };
    for key in drop {
        if used(&hints) <= width as usize {
            break;
        }
        hints.retain(|(k, _, _)| k != key);
    }
    hints
}

fn render_list(frame: &mut Frame, app: &mut App, area: Rect) -> Hits {
    if app.detail_full {
        return detail::render(frame, app, area);
    }
    let detail_height = detail_height(area.height, roster::wanted_height(app));
    let [grid_area, detail_area] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(detail_height)]).areas(area);

    let mut hits = roster::render(frame, app, grid_area);
    hits.extend(detail::render(frame, app, detail_area));
    hits
}

/// How many lines of `height` the details pane gets under a list that wants `list` lines: half,
/// from 6 to 16, plus every line the list leaves empty.
fn detail_height(height: u16, list: u16) -> u16 {
    let half = (height / 2).clamp(6, 16);
    half.max(height.saturating_sub(list)).min(height)
}

/// Shorten to `width` display columns, marking the cut with an ellipsis.
pub(crate) fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    match width {
        0 => String::new(),
        1 => "…".into(),
        _ => text.chars().take(width - 1).collect::<String>() + "…",
    }
}

/// Break a paragraph at word boundaries. An empty line stays one empty line.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() || text.chars().count() <= width {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let extra = if line.is_empty() { 0 } else { 1 };
        if !line.is_empty() && line.chars().count() + extra + word.chars().count() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The shared panel chrome: a heavy rule above, a light one below, no side walls, two columns of
/// inner padding, and `title` in the top rule.
pub(crate) fn panel(title: &str, focused: bool) -> Block<'static> {
    bordered(focused).title(Span::styled(format!(" {title} "), theme::title()))
}

/// The rules of a [`panel`]: heavy on top, light below.
pub(crate) const RULES: symbols::border::Set = symbols::border::Set {
    horizontal_top: "━",
    ..symbols::border::PLAIN
};

/// [`panel`] without a title, for a pane that draws its own top rule (the details pane's tabs).
pub(crate) fn bordered(focused: bool) -> Block<'static> {
    Block::new()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_set(RULES)
        .border_style(if focused {
            theme::border_focused()
        } else {
            theme::border()
        })
        .padding(Padding::horizontal(2))
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;
    use crate::app::{CreateRow, KitsForm, Modal};
    use crate::create::{CreateStep, WizardRow};
    use crate::input::KeyEvent;
    use crate::kits::Catalog;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    pub use aisbx_core::testkit::{auth_form, held, sample_secrets};

    fn key(c: char) -> KeyEvent {
        KeyEvent::from(KeyCode::Char(c))
    }

    /// Every screen rendered to plain text at a comfortable size.
    pub fn screens() -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut app = App::for_test(Catalog::load().unwrap(), super::tests::populated());
        out.push(("List".to_string(), render(&mut app)));
        // The drifted sandbox selected, for its note on the row and in the details pane.
        app.cursor = 1;
        out.push(("List — update ready".to_string(), render(&mut app)));
        app.cursor = app.here;
        out.push((
            "List — cursor on + New sandbox".to_string(),
            render(&mut app),
        ));
        app.cursor = 0;
        app.on_key(key('a'));
        out.push((
            "List — other folders collapsed".to_string(),
            render(&mut app),
        ));
        app.on_key(key('a'));
        app.cursor = 0;

        // A folder without sandboxes: the others are still under the second heading.
        let mut empty = App::for_test(Catalog::load().unwrap(), super::tests::populated());
        empty.cwd = std::path::PathBuf::from("/elsewhere");
        empty.refresh_cells();
        out.push((
            "List — folder with no sandboxes".to_string(),
            render(&mut empty),
        ));
        // Wide enough for the Overview's record to stand in a second column.
        out.push((
            "List — wide terminal".to_string(),
            render_at(&mut app, 180, 34),
        ));
        // One column, and short enough that the second section is only partly on screen.
        out.push((
            "List — narrow terminal".to_string(),
            render_at(&mut app, 52, 24),
        ));

        // A request held for approval, for the header badge, the row's marker and the Overview row.
        app.approvals = vec![held("claude-anthropic-proj", "registry.npmjs.org:443")];
        app.cursor = 0;
        out.push((
            "List — a request held for approval".to_string(),
            render(&mut app),
        ));
        app.on_key(key('y'));
        out.push(("Approvals".to_string(), render(&mut app)));
        app.approvals
            .push(held("opencode-proj", "GET api.example.com:443/v1/data"));
        out.push(("Approvals — two sandboxes".to_string(), render(&mut app)));
        app.approvals.clear();
        out.push(("Approvals — nothing waiting".to_string(), render(&mut app)));
        app.screen = Screen::List;

        // Kept running, with a published port and recorded limits.
        app.sandboxes[0].keep_running = true;
        app.sandboxes[0].cpus = Some(4);
        app.sandboxes[0].memory = Some("8g".into());
        app.sandboxes[0].ports = vec![crate::sbx::Port {
            host_ip: "127.0.0.1".into(),
            host_port: 8080,
            sandbox_port: 3000,
            protocol: "tcp4".into(),
            ephemeral: false,
        }];
        out.push((
            "List — kept running in the background".to_string(),
            render(&mut app),
        ));
        app.on_key(key('b'));
        out.push((
            "Keep running — undoing it is a rebuild".to_string(),
            render(&mut app),
        ));
        app.modal = None;
        app.sandboxes[0].keep_running = false;
        app.on_key(key('b'));
        out.push((
            "Keep running — the confirmation".to_string(),
            render(&mut app),
        ));
        app.modal = None;
        app.sandboxes[0].cpus = None;
        app.sandboxes[0].memory = None;
        app.sandboxes[0].ports.clear();

        // The host-wide sweep's fallback body: tests do not fetch the `sbx prune --dry-run` table.
        app.on_key(key('P'));
        out.push(("Prune — the confirmation".to_string(), render(&mut app)));
        app.modal = None;

        app.insight = crate::insight::Insight::sample("claude-anthropic-proj");
        for tab in crate::insight::Tab::ALL {
            app.select_tab(tab.index());
            out.push((format!("Details — {}", tab.title()), render(&mut app)));
        }
        app.detail_full = true;
        app.select_tab(crate::insight::Tab::Network.index());
        out.push(("Details — maximized".to_string(), render(&mut app)));
        app.detail_full = false;
        app.select_tab(0);

        // The create wizard, one step at a time.
        app.open_create();
        for step in CreateStep::ALL {
            if let Screen::Create(form) = &mut app.screen {
                form.wizard_set_step(&app.catalog, step);
            }
            out.push((format!("Create — {}", step.title()), render(&mut app)));
        }
        // The busiest step at 80x24.
        if let Screen::Create(form) = &mut app.screen {
            form.wizard_set_step(&app.catalog, CreateStep::Workspace);
        }
        out.push(("Create — 80×24".to_string(), render_at(&mut app, 80, 24)));
        // The same without colour or Unicode (`NO_COLOR=1 LC_ALL=C`).
        let plain = crate::theme::Caps {
            color: false,
            unicode: false,
        };
        out.push((
            "Create — 80×24, NO_COLOR and LC_ALL=C".to_string(),
            render_with(&mut app, 80, 24, plain),
        ));
        // And the Ports row, with its explanation in the strip.
        if let Screen::Create(form) = &mut app.screen {
            form.focus_field(&app.catalog, CreateRow::Ports);
        }
        out.push((
            "Create — the ports field".to_string(),
            render_at(&mut app, 80, 24),
        ));
        // The shared-skills row, which has three answers, not two.
        if let Screen::Create(form) = &mut app.screen {
            form.focus_field(&app.catalog, CreateRow::Skills);
            form.workspace.skills = crate::sbx::SkillsMode::Readwrite;
        }
        out.push((
            "Create — the skills mount".to_string(),
            render_at(&mut app, 80, 24),
        ));
        if let Screen::Create(form) = &mut app.screen {
            form.workspace.skills = crate::sbx::SkillsMode::default();
            // The button line, where the footer offers left/right, not `space`.
            form.focus_wizard_row(&app.catalog, WizardRow::Next);
        }
        out.push((
            "Create — the buttons".to_string(),
            render_at(&mut app, 80, 24),
        ));
        if let Screen::Create(form) = &mut app.screen {
            form.wizard_set_step(&app.catalog, CreateStep::Harness);
        }
        // The Connection step on a host without a LiteLLM connection that has a key.
        app.secrets = Some(crate::sbx::Secrets::default());
        out.push(("Create — no connection key".to_string(), render(&mut app)));
        app.secrets = None;
        // *new connection…*, half typed: its fields under the row, the key masked.
        if let Screen::Create(form) = &mut app.screen {
            let pick = form.connection.litellm.take();
            form.connection.new_conn.url = crate::app::TextInput::new("https://gw.example.org");
            form.connection.new_conn.key = crate::app::TextInput::new("not-a-real-key");
            out.push(("Create — new connection".to_string(), render(&mut app)));
            if let Screen::Create(form) = &mut app.screen {
                form.connection.litellm = pick;
            }
        }
        // The gateway backend's model pins: one alias's list open, another alias already pinned.
        let an_opus = app
            .catalog
            .models
            .iter()
            .find(|m| m.id.contains("opus"))
            .or_else(|| app.catalog.models.first())
            .cloned();
        if let Some(model) = an_opus.clone() {
            if let Screen::Create(form) = &mut app.screen {
                form.connection
                    .models
                    .set(crate::models::Slot::Opus, Some(model.id));
                form.connection.model_pick = Some(crate::models::Slot::Sonnet);
            }
            out.push(("Create — Claude models".to_string(), render(&mut app)));
            if let Screen::Create(form) = &mut app.screen {
                form.connection.models = Default::default();
                form.connection.model_pick = None;
            }
        }
        // The account screens need the subscription backend; the gateway one has no account.
        let subscription = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account().is_some())
            .unwrap_or(0);
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = subscription;
            // Setting the backend directly skips the label update that the key handlers do.
            form.workspace.name = crate::app::TextInput::new("Claude Code");
            form.connection.tokens = vec!["private".into(), "work".into()];
            form.connection.account = crate::app::AccountChoice::Named("private".into());
        }
        out.push((
            "Create — an existing setup token".to_string(),
            render(&mut app),
        ));
        if let Screen::Create(form) = &mut app.screen {
            form.connection.account = crate::app::AccountChoice::New;
            form.connection.token_name = crate::app::TextInput::new("personal");
            form.connection.token = crate::app::TextInput::new("sk-ant-oat01-0123456789");
        }
        out.push(("Create — a new setup token".to_string(), render(&mut app)));

        for (title, account) in [
            ("/login", crate::app::AccountChoice::Login),
            (
                "an existing token",
                crate::app::AccountChoice::Named("work".into()),
            ),
            ("a new token", crate::app::AccountChoice::New),
        ] {
            app.screen = Screen::AuthEdit(auth_form(account));
            out.push((format!("Auth — {title}"), render(&mut app)));
        }

        let addons: Vec<bool> = app
            .catalog
            .addons
            .iter()
            .map(|a| a.kit.name == "rtk")
            .collect();
        app.screen = Screen::Kits(KitsForm {
            sandbox: "claude-anthropic-proj".into(),
            original: addons.clone(),
            addons,
            // A subscription sandbox has no gateway models to pin.
            models_offered: false,
            models: Default::default(),
            original_models: Default::default(),
            model_pick: None,
            focus: 0,
            ram_volumes: false,
            history_in_volumes: true,
            harness: None,
        });
        out.push(("Kits".to_string(), render(&mut app)));

        // The same screen for a gateway sandbox, which also pins its models.
        if let Some(model) = an_opus {
            let addons: Vec<bool> = app.catalog.addons.iter().map(|_| false).collect();
            let mut models = crate::models::Overrides::default();
            models.set(crate::models::Slot::Opus, Some(model.id));
            app.screen = Screen::Kits(KitsForm {
                sandbox: "claude-litellm-proj".into(),
                original: addons.clone(),
                addons,
                models_offered: true,
                original_models: Default::default(),
                models,
                model_pick: None,
                focus: 0,
                ram_volumes: false,
                history_in_volumes: true,
                harness: None,
            });
            out.push(("Kits — Claude models".to_string(), render(&mut app)));
        }

        app.secrets = Some(sample_secrets());
        app.screen = Screen::Secrets(crate::app::SecretsForm::default());
        out.push(("Secrets".to_string(), render(&mut app)));
        // The row-level explanation moves with the cursor, so preview one further down too.
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = 1;
        }
        out.push(("Secrets — a named token".to_string(), render(&mut app)));
        // And on an `mcp:` row, whose note differs from a kit credential's.
        let mcp = app
            .secret_rows()
            .iter()
            .position(|r| {
                matches!(r, crate::app::SecretRow::Service { name, .. }
                    if name.starts_with("mcp:slack"))
            })
            .unwrap_or(0);
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = mcp;
        }
        out.push((
            "Secrets — an MCP gateway credential".to_string(),
            render(&mut app),
        ));

        // The shared skills store: one skill installed, and the three commands that change it.
        app.skills = Some(crate::sbx::SkillsStore {
            store:
                "/Users/u/Library/Application Support/com.docker.sandboxes/sandboxes/agent-skills"
                    .into(),
            skills: vec!["frontend-design".into(), "pdf".into()],
        });
        app.screen = Screen::Skills(crate::app::SkillsForm::default());
        out.push(("Skills".to_string(), render(&mut app)));
        if let Screen::Skills(form) = &mut app.screen {
            form.focus = 3;
        }
        out.push((
            "Skills — add from a repository".to_string(),
            render(&mut app),
        ));
        app.skills = Some(crate::sbx::SkillsStore::default());
        app.screen = Screen::Skills(crate::app::SkillsForm::default());
        out.push(("Skills — empty store".to_string(), render(&mut app)));

        // Connecting an editor: a launchable target and one configured inside the app.
        for (title, focus) in [("terminal", 0usize), ("IntelliJ", 3)] {
            app.screen = Screen::Connect(crate::app::ConnectForm {
                sandbox: "claude-anthropic-proj".into(),
                ssh_ready: true,
                focus,
            });
            out.push((format!("Connect — {title}"), render(&mut app)));
        }
        // …and the state a first run is in, where nothing below can connect yet.
        app.screen = Screen::Connect(crate::app::ConnectForm {
            sandbox: "claude-anthropic-proj".into(),
            ssh_ready: false,
            focus: 0,
        });
        out.push(("Connect — SSH not set up".to_string(), render(&mut app)));

        // The start-folder picker reads the real filesystem, so it needs a tree that exists.
        let ws = start_dir_tree();
        app.sandboxes[0].workspace = ws.clone();
        for (title, rel) in [
            ("at the workspace root", None),
            ("one level in", Some(std::path::PathBuf::from("notes"))),
        ] {
            app.screen = Screen::StartDir(crate::app::StartDirForm::open(
                crate::app::StartDirTarget::Sandbox("claude-anthropic-proj".into()),
                "Claude Code KI".into(),
                ws.clone(),
                rel,
            ));
            out.push((format!("Start folder — {title}"), render(&mut app)));
        }
        // …and the button that commits, which is where the two ways to apply it are spelled out.
        if let Screen::StartDir(form) = &mut app.screen {
            let rows = form.rows();
            form.focus = rows
                .iter()
                .position(|r| *r == crate::app::StartDirRow::Start)
                .unwrap_or(0);
        }
        out.push(("Start folder — the buttons".to_string(), render(&mut app)));

        app.screen = Screen::List;
        app.modal = Some(Modal::Prompt(crate::app::Prompt {
            ask: crate::app::Ask::PublishPort,
            subject: "claude-anthropic-proj".into(),
            input: crate::app::TextInput::new("8080:3000"),
            help: Vec::new(),
        }));
        out.push(("Publish a port".to_string(), render(&mut app)));
        app.modal = Some(Modal::Help);
        out.push(("Help".to_string(), render(&mut app)));
        out
    }

    /// A workspace holding several projects — the shape the start folder exists for.
    fn start_dir_tree() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("aisbx-preview-{}", std::process::id()));
        for rel in [
            "aisbx",
            "notes/drafts",
            "notes/published",
            "scripts",
            ".git",
        ] {
            std::fs::create_dir_all(root.join(rel)).unwrap();
        }
        root
    }

    pub fn render(app: &mut App) -> String {
        render_at(app, 110, 34)
    }

    pub fn render_at(app: &mut App, width: u16, height: u16) -> String {
        render_with(app, width, height, crate::theme::Caps::FULL)
    }

    /// [`render_at`] for a terminal that can show `caps`.
    pub fn render_with(app: &mut App, width: u16, height: u16, caps: crate::theme::Caps) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw_in(frame, app, caps)).unwrap();
        terminal.backend().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        Ask, Confirm, CreateRow, KitsForm, Modal, PendingJob, PostJob, Prompt, TextInput,
    };
    use crate::create::{ConnChoice, CreateStep, WizardRow};
    use crate::job::Job;
    use crate::kits::Catalog;
    use crate::model::Sandbox;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// Terminal sizes worth surviving: comfortable, default, cramped, and absurd.
    const SIZES: &[(u16, u16)] = &[(140, 45), (100, 30), (80, 24), (46, 14), (20, 8)];

    fn app_with(sandboxes: Vec<Sandbox>) -> App {
        let mut app = App::for_test(
            Catalog::load().expect("kits/ must be discoverable from the crate"),
            sandboxes,
        );
        // The renders get a fixed model list, as a LiteLLM connection would offer it.
        let catalog = std::sync::Arc::make_mut(&mut app.catalog);
        if catalog.models.is_empty() {
            catalog.models = [
                ("claude-opus-5", "Claude Opus 5"),
                ("claude-sonnet-5", "Claude Sonnet 5"),
                ("glm-5", "GLM 5"),
            ]
            .iter()
            .map(|(id, name)| crate::models::Model {
                id: id.to_string(),
                name: name.to_string(),
                ..Default::default()
            })
            .collect();
        }
        app
    }

    #[test]
    fn the_folders_screen_lists_each_folder_with_its_access_and_purpose() {
        let state =
            std::env::temp_dir().join(format!("aisbx-tests-tui-folders-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        let entry = crate::folders::Entry {
            path: "/srv/kb".into(),
            access: crate::folders::Access::Write,
            purpose: "the knowledge base".into(),
        };
        crate::folders::put(&state, entry).unwrap();
        let mut app = app_with(populated());
        app.mounts_state = state.clone();
        app.on_key(KeyEvent::from(KeyCode::Char('F')));
        assert!(matches!(app.screen, Screen::Folders(_)));
        let screen = super::tests_support::render_at(&mut app, 80, 24);
        for text in [
            "Host folders",
            "/srv/kb",
            "read-write",
            "the knowledge base",
            "[ Browse… ]",
            "[ Save ]",
        ] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        render_all_sizes(&mut app);
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn the_host_folder_picker_names_the_browsed_folder_and_offers_use() {
        let tmp =
            std::env::temp_dir().join(format!("aisbx-tests-tui-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("kb")).unwrap();
        let mut app = app_with(populated());
        app.screen = Screen::StartDir(crate::app::StartDirForm::open(
            crate::app::StartDirTarget::HostFolder(Box::default()),
            "host folder".into(),
            tmp.clone(),
            None,
        ));
        let screen = super::tests_support::render_at(&mut app, 100, 30);
        let kb = model::tilde(&tmp.join("kb"));
        for text in ["Pick a host folder", "→  kb", &kb, "[ Use ]", "Cancel"] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        assert!(!screen.contains("Start here"), "{screen}");
        assert!(!screen.contains("Opens in"), "{screen}");
        assert!(
            !screen.contains(" › kb"),
            "the whole path, not root › rest: {screen}"
        );
        render_all_sizes(&mut app);
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn a_read_write_folder_stands_out_in_the_warning_colour() {
        use crate::folders::{Access, Entry};
        let mut app = app_with(populated());
        let form = crate::app::FoldersForm {
            folders: vec![
                Entry {
                    path: "/srv/kb".into(),
                    access: Access::Read,
                    purpose: "notes".into(),
                },
                Entry {
                    path: "/srv/out".into(),
                    access: Access::Write,
                    purpose: "reports".into(),
                },
            ],
            access: Access::Write,
            focus: 2,
            ..Default::default()
        };
        app.screen = Screen::Folders(form);
        let buffer = buffer_at(&mut app, 110, 34);
        let colour = |path: &str, access: &str| {
            let (_, y) = lines_with(&buffer, path)[0];
            let x = column_of(&buffer, y, access).expect(access);
            buffer[(x, y)].fg
        };
        assert_eq!(colour("/srv/kb", "read-only"), theme::MUTED);
        assert_eq!(colour("/srv/out", "read-write"), theme::WARN);
        // The access row's pick says it too, while read-write is picked.
        assert_eq!(colour("◉ read-write", "◉ read-write"), theme::WARN);
    }

    fn render_all_sizes(app: &mut App) {
        for (w, h) in SIZES {
            let mut terminal = Terminal::new(TestBackend::new(*w, *h)).unwrap();
            terminal.draw(|frame| draw(frame, app)).unwrap();
        }
    }

    /// Two sandboxes in the current folder plus one that belongs elsewhere, so every render goes
    /// through both sections.
    pub(super) fn populated() -> Vec<Sandbox> {
        let mut a = model::sample(
            "claude-anthropic-proj",
            Some("claude-anthropic"),
            &["jdk25", "rtk"],
        );
        a.label = "My private Claude Code".into();
        a.running = true;
        a.sessions = 1;
        let mut b = model::sample(
            "opencode-proj",
            Some("opencode-litellm"),
            &["web", "host-gradle", "host-maven"],
        );
        b.label = "OpenCode (Web, Host Gradle, Host Maven)".into();
        b.agent = "opencode".into();
        b.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        b.ro_mounts = vec![std::path::PathBuf::from("/home/u/.m2/repository")];
        // Stopped three days ago, relative to now, or the preview would age.
        b.last_used = Some(crate::state::now_epoch().saturating_sub(3 * 86_400));
        let mut c = model::sample("ts-lab", None, &[]);
        c.workspace = std::path::PathBuf::from("/gone");
        c.workspace_missing = true;
        c.last_used = Some(crate::state::now_epoch().saturating_sub(28 * 86_400));
        vec![a, b, c]
    }

    #[test]
    fn panels_are_ruled_above_and_below_without_side_walls() {
        let mut app = app_with(populated());
        let screen = super::tests_support::render_at(&mut app, 110, 34);
        let lines: Vec<&str> = screen.lines().map(|l| l.trim_matches('"')).collect();
        assert!(lines[1].starts_with(" Sandboxes ━━"), "{screen}");
        assert!(
            lines.iter().any(|l| l.starts_with("━━ 1 Overview")),
            "{screen}"
        );
        assert!(
            !screen.contains(['│', '╭', '╮', '╰', '╯']),
            "no side walls, no corners: {screen}"
        );
    }

    #[test]
    fn renders_the_list_at_every_size() {
        let mut app = app_with(populated());
        render_all_sizes(&mut app);
        app.on_key(KeyEvent::from(KeyCode::Char('a'))); // other folders collapsed
        render_all_sizes(&mut app);
    }

    #[test]
    fn the_footer_offers_other_folders_only_while_another_folder_has_a_sandbox() {
        let footer = |app: &mut App| {
            let screen = super::tests_support::render_at(app, 240, 30);
            screen.lines().last().unwrap().to_string()
        };
        let mut app = app_with(populated());
        for cell in [0, app.here] {
            app.select(cell);
            assert!(footer(&mut app).contains("a hide others"), "{cell}");
        }
        let here: Vec<Sandbox> = (populated().into_iter())
            .filter(|sb| sb.name != "ts-lab")
            .collect();
        let mut app = app_with(here);
        assert_eq!(app.elsewhere, 0);
        for cell in [0, app.here] {
            app.select(cell);
            let line = footer(&mut app);
            assert!(!line.contains(" a "), "{cell}: {line}");
            assert!(!line.contains("others"), "{cell}: {line}");
            assert!(line.contains("/ find"), "{cell}: {line}");
        }
    }

    #[test]
    fn renders_the_empty_state() {
        let mut app = crate::app::App::for_test(crate::kits::Catalog::load().unwrap(), Vec::new());
        render_all_sizes(&mut app);
    }

    #[test]
    fn the_details_pane_takes_what_the_list_leaves() {
        assert_eq!(detail_height(32, 12), 20, "a short list gives up its slack");
        assert_eq!(detail_height(32, 40), 16, "a long list keeps the old split");
        assert_eq!(detail_height(12, 20), 6);
        assert_eq!(detail_height(12, 4), 8);
        assert_eq!(detail_height(5, 2), 5, "never more than there is");
    }

    /// The y of the details pane's top border: the second panel's, under the list's — its tabs
    /// on a sandbox, *New sandbox* on the New row.
    fn details_top(screen: &str) -> usize {
        let tops = screen.lines().enumerate().filter(|(_, l)| {
            let l = l.trim_start_matches('"');
            l.starts_with("━━ 1 Overview") || l.starts_with(" New sandbox ━")
        });
        let tops: Vec<usize> = tops.map(|(y, _)| y).collect();
        assert_eq!(tops.len(), 1, "one details pane:\n{screen}");
        tops[0]
    }

    #[test]
    fn a_short_list_leaves_the_details_room_and_the_split_never_moves() {
        let mut app = app_with(populated());
        app.insight = crate::insight::Insight::sample("claude-anthropic-proj");
        let screen = super::tests_support::render_at(&mut app, 110, 34);
        let top = details_top(&screen);
        // The list takes what it wants and no more: the header line, then its panel.
        assert_eq!(top, 1 + roster::wanted_height(&app) as usize, "{screen}");
        assert!(top < 1 + 16, "more than the old half: {screen}");
        assert!(
            !screen.contains("more  ctrl-↑↓ scroll"),
            "the Overview fits:\n{screen}"
        );
        for _ in 0..app.cells.len() {
            app.on_key(KeyEvent::from(KeyCode::Down));
            let screen = super::tests_support::render_at(&mut app, 110, 34);
            assert_eq!(details_top(&screen), top, "{screen}");
        }
        app.cursor = 0;
        app.approvals = vec![super::tests_support::held(
            "ts-lab",
            "registry.npmjs.org:443",
        )];
        let screen = super::tests_support::render_at(&mut app, 110, 34);
        assert!(screen.contains("to approve"), "{screen}");
        assert_eq!(details_top(&screen), top, "{screen}");
    }

    #[test]
    fn renders_the_create_form_including_its_last_row() {
        let mut app = app_with(populated());
        app.open_create();
        for step in CreateStep::ALL {
            show_step(&mut app, step);
            render_all_sizes(&mut app);
            // Walk the focus through every row — the step scrolls and the buttons must not panic.
            for _ in 0..40 {
                app.on_key(KeyEvent::from(KeyCode::Down));
                render_all_sizes(&mut app);
            }
            // …and along the button line, which ↓ steps over rather than into.
            app.on_key(KeyEvent::from(KeyCode::Up));
            for _ in 0..4 {
                app.on_key(KeyEvent::from(KeyCode::Right));
                render_all_sizes(&mut app);
            }
        }
    }

    #[test]
    fn the_create_form_offers_blocked_hosts_and_only_offers_mcp_when_there_is_any() {
        let mut app = app_with(populated());
        app.open_create();
        show_step(&mut app, CreateStep::Workspace);

        // No MCP servers registered — the section is not on the form at all.
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("blocked hosts"), "{screen}");
        assert!(
            !screen.contains("MCP servers"),
            "nothing registered, so nothing offered: {screen}"
        );
        assert!(
            screen.contains("[ Next ]"),
            "the step still reaches its buttons: {screen}"
        );

        // The note is drawn while the field holds focus, and says which way the rule goes.
        focus_row(&mut app, CreateRow::DenyNetwork);
        let screen = super::tests_support::render_at(&mut app, 80, 24);
        assert!(screen.contains("may not reach"), "{screen}");
        assert!(
            screen.contains("beats every allow"),
            "why a kit cannot undo it: {screen}"
        );

        // With servers registered, each is a checkbox and `--static-mcp` picks them up.
        app.mcp = vec![
            crate::sbx::McpServer {
                name: "notion".into(),
                transport: "remote http".into(),
                status: "needs auth".into(),
                kind: "remote".into(),
            },
            crate::sbx::McpServer {
                name: "gh".into(),
                transport: "local stdio".into(),
                status: "ready".into(),
                kind: "local".into(),
            },
        ];
        app.open_create();
        show_step(&mut app, CreateStep::Workspace);
        let screen = super::tests_support::render_at(&mut app, 110, 44);
        assert!(screen.contains("MCP servers"), "{screen}");
        assert!(
            screen.contains("notion") && screen.contains("gh"),
            "{screen}"
        );

        let Screen::Create(form) = &mut app.screen else {
            unreachable!()
        };
        assert!(form.selected_mcp().is_empty(), "nothing ticked by default");
        form.workspace.static_mcp[0] = true;
        assert_eq!(form.selected_mcp(), vec!["notion"]);
    }

    /// Put the create wizard on the step that owns `want`, with its cursor on it, found by identity,
    /// not by a count. Returns where the row sits among the step's rows, as its click target carries.
    fn focus_row(app: &mut App, want: CreateRow) -> usize {
        let catalog = std::sync::Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            unreachable!("the create form is open")
        };
        assert!(
            form.focus_field(&catalog, want),
            "the create form offers {want:?}"
        );
        form.wizard_focus()
    }

    /// Put the create wizard on `step`, whatever the answers say.
    fn show_step(app: &mut App, step: CreateStep) {
        let catalog = std::sync::Arc::clone(&app.catalog);
        if let Screen::Create(form) = &mut app.screen {
            form.wizard_set_step(&catalog, step);
        }
    }

    /// Where `row` sits among the wizard's rows on the step it is on.
    fn wizard_index(app: &App, row: WizardRow) -> usize {
        wizard_rows(app)
            .iter()
            .position(|r| *r == row)
            .unwrap_or_else(|| panic!("{row:?} is on this step"))
    }

    #[test]
    fn the_ports_field_and_its_note_land_on_screen_together() {
        let mut app = app_with(populated());
        app.open_create();

        // Unfocused, it is a field of its own, under its own heading, with a marker and a value.
        show_step(&mut app, CreateStep::Workspace);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("Network"), "{screen}");
        assert!(screen.contains("ports"), "{screen}");
        assert!(
            screen.contains("(none)"),
            "empty means no ports, not (auto): {screen}"
        );
        assert!(screen.contains("[ Next ]"), "{screen}");
        assert!(
            screen.contains("opens in"),
            "where the agent starts is on the form without being hunted for: {screen}"
        );
        show_step(&mut app, CreateStep::Kits);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(
            screen.contains("Host skills"),
            "every addon row is on screen with the buttons: {screen}"
        );
        assert!(screen.contains("headroom"), "{screen}");
        assert!(screen.contains("[ Next ]"), "{screen}");

        focus_row(&mut app, CreateRow::Ports);
        let screen = super::tests_support::render_at(&mut app, 80, 24);
        let strip = strip_text(&screen);
        assert!(strip.contains("8080:3000 binds host port 8080"), "{screen}");
        // The end of the sentence, not just the part that fitted the terminal it was written for.
        assert!(strip.ends_with("not 127.0.0.1."), "{screen}");
    }

    #[test]
    fn a_ticked_headroom_row_warns_by_backend() {
        let mut app = app_with(populated());
        app.open_create();
        show_step(&mut app, CreateStep::Kits);
        let headroom = app
            .catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "headroom")
            .expect("the create form offers headroom");
        let opencode = app
            .catalog
            .backends
            .iter()
            .position(|b| b.kit.name == "opencode-litellm")
            .expect("the OpenCode backend is offered");

        if let Screen::Create(form) = &app.screen {
            assert_eq!(
                app.catalog.backends[form.harness.backend].kit.name,
                "claude-litellm"
            );
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(!screen.contains("headroom is unmeasured"), "{screen}");
        assert!(!screen.contains("routes only Claude Code"), "{screen}");

        // The default backend, claude-litellm, runs Claude Code.
        if let Screen::Create(form) = &mut app.screen {
            form.kits.addons[headroom] = true;
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("headroom is unmeasured"), "{screen}");
        assert!(!screen.contains("routes only Claude Code"), "{screen}");

        // headroom works with Claude Code only, so OpenCode's form does not offer it.
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = opencode;
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(!screen.contains("headroom"), "{screen}");
        assert!(!screen.contains("routes only Claude Code"), "{screen}");

        if let Screen::Create(form) = &mut app.screen {
            form.kits.addons[headroom] = false;
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(!screen.contains("headroom is unmeasured"), "{screen}");
        assert!(!screen.contains("routes only Claude Code"), "{screen}");
    }

    #[test]
    fn a_held_request_is_on_the_header_the_card_and_the_approvals_screen() {
        let mut app = app_with(populated());
        let quiet = super::tests_support::render(&mut app);
        assert!(!quiet.contains('⚑'), "nothing waiting, nothing drawn");

        app.approvals = vec![super::tests_support::held(
            "claude-anthropic-proj",
            "registry.npmjs.org:443",
        )];
        app.cursor = 0;
        let list = super::tests_support::render(&mut app);
        assert!(list.contains("⚑ 1 approval waiting"), "{list}");
        assert!(
            list.contains("to approve"),
            "the card says what is waiting: {list}"
        );
        assert!(
            list.contains("registry.npmjs.org:443 waits for approval — y to answer"),
            "the Overview row names where the agent tried to go: {list}"
        );
        assert!(
            list.contains("y approvals"),
            "and the footer offers the key: {list}"
        );

        app.on_key(KeyEvent::from(KeyCode::Char('y')));
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("registry.npmjs.org:443"), "{screen}");
        assert!(
            screen.contains("no matching allow rule (default deny)"),
            "{screen}"
        );
        assert!(
            screen.contains("My private Claude Code"),
            "grouped under its label: {screen}"
        );
        render_all_sizes(&mut app);

        app.approvals.clear();
        let empty = super::tests_support::render(&mut app);
        assert!(empty.contains("Nothing is waiting"), "{empty}");
        render_all_sizes(&mut app);
    }

    #[test]
    fn a_refresh_redraws_only_its_mark() {
        let mut app = app_with(populated());
        app.approvals = vec![super::tests_support::held(
            "claude-anthropic-proj",
            "registry.npmjs.org:443",
        )];
        for (w, h) in SIZES {
            let frame = |app: &mut App| {
                let mut terminal = Terminal::new(TestBackend::new(*w, *h)).unwrap();
                terminal.draw(|frame| draw(frame, app)).unwrap();
                terminal.backend().buffer().clone()
            };
            app.loading = false;
            let idle = frame(&mut app);
            assert!(
                idle.diff(&frame(&mut app)).is_empty(),
                "{w}×{h}: one state, one frame"
            );
            app.loading = true;
            let refreshing = frame(&mut app);
            let changed: Vec<String> = idle
                .diff(&refreshing)
                .iter()
                .map(|(x, y, cell)| format!("{x},{y} {}", cell.symbol()))
                .collect();
            assert!(changed.len() <= 1, "{w}×{h}: {changed:?}");
        }
    }

    #[test]
    fn the_status_line_counts_the_running_sandboxes_where_it_has_room() {
        let first = |app: &mut App, w: u16| {
            let screen = super::tests_support::render_at(app, w, 24);
            screen.lines().next().unwrap_or_default().to_string()
        };
        let mut app = app_with(populated());
        app.sandboxes[1].running = true;
        let all = app.sandboxes.len();
        let running = app.sandboxes.iter().filter(|sb| sb.running).count();
        assert!(running > 0 && running < all, "a mixed host");
        let summary = format!("● {running} of {all} running");
        assert!(
            first(&mut app, 110).contains(&summary),
            "{}",
            first(&mut app, 110)
        );
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(!app.show_other);
        assert!(
            first(&mut app, 110).contains(&summary),
            "folded, still counted"
        );

        app.approvals = vec![super::tests_support::held(
            "claude-anthropic-proj",
            "registry.npmjs.org:443",
        )];
        // Left out whole, not half drawn over by the right side.
        let narrow = first(&mut app, 60);
        assert!(
            !narrow.contains('●') && !narrow.contains("running"),
            "{narrow}"
        );
        for kept in ["aisbx", "⚑ 1 approval waiting", "sbx "] {
            assert!(narrow.contains(kept), "{kept}: {narrow}");
        }

        for sb in &mut app.sandboxes {
            sb.running = false;
        }
        let stopped = first(&mut app, 110);
        assert!(
            stopped.contains(&format!("○ 0 of {all} running")),
            "{stopped}"
        );
        let mut empty = app_with(Vec::new());
        assert!(!first(&mut empty, 110).contains("running"));
    }

    #[test]
    fn an_unresponsive_sandbox_says_so_first() {
        let mut sandboxes = populated();
        sandboxes[0].unresponsive = true;
        sandboxes[0].drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        let mut app = app_with(sandboxes);
        app.cursor = 0;
        let list = super::tests_support::render(&mut app);
        assert!(list.contains("not responding"), "{list}");
    }

    #[test]
    fn renders_the_secrets_screen() {
        let mut app = app_with(populated());
        app.secrets = Some(super::tests_support::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        render_all_sizes(&mut app);
        // Every row's explanation is drawn in the strip, and the rows scroll to keep the cursor's
        // in view.
        for _ in 0..app.secret_rows().len() + 1 {
            app.on_key(KeyEvent::from(KeyCode::Down));
            render_all_sizes(&mut app);
        }

        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("/login (OAuth)"), "{screen}");
        assert!(screen.contains("token private"), "{screen}");
        assert!(screen.contains("litellm"), "{screen}");
        // Masked, as sbx prints them — never a value.
        assert!(screen.contains("sk-ant********"), "{screen}");
        // The longest name keeps a gap before its value.
        let screen_tall = super::tests_support::render_at(&mut app, 110, 50);
        assert!(
            screen_tall.contains("mcp:slack:client_secret   sk***"),
            "{screen_tall}"
        );
        // What the rows hide below is counted on the strip's rule.
        assert!(screen.contains("more"), "{screen}");
        // Somebody else's custom secret is shown, and labelled as not ours.
        let screen = super::tests_support::render_at(&mut app, 110, 50);
        assert!(screen.contains("someone-elses-secret"), "{screen}");
        assert!(screen.contains("not aisbx's"), "{screen}");

        // A host with nothing stored says which credentials are missing rather than listing none.
        let mut empty = app_with(Vec::new());
        empty.secrets = Some(crate::sbx::Secrets::default());
        empty.on_key(KeyEvent::from(KeyCode::Char('S')));
        let screen = super::tests_support::render(&mut empty);
        assert!(screen.contains("⚠ not stored"), "{screen}");
    }

    #[test]
    fn the_secrets_screen_lists_the_litellm_connections() {
        let mut app = app_with(populated());
        app.secrets = Some(super::tests_support::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        let at = app
            .secret_rows()
            .iter()
            .position(|r| *r == crate::app::SecretRow::Connection("acme".into()))
            .expect("the connection is on the screen");
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = at;
        }
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("LiteLLM connections"), "{screen}");
        assert!(
            screen.contains("litellm.example.com · 2 models"),
            "{screen}"
        );
        assert!(screen.contains("sk-l********"), "{screen}");
        assert!(screen.contains("no connection.json names it"), "{screen}");
        assert!(
            strip_text(&screen).contains("Edit changes its URL or key, Delete removes both"),
            "{screen}"
        );
        assert!(screen.contains("n new connection"), "{screen}");
        assert!(screen.contains("↵ edit"), "{screen}");

        // A key stored for another host is not this connection's key.
        app.secrets = Some(crate::sbx::Secrets {
            services: Vec::new(),
            custom: vec![aisbx_core::testkit::litellm_key("acme", "old.example.com")],
        });
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("⚠ not stored"), "{screen}");
        assert!(screen.contains("key stored for another host"), "{screen}");
    }

    #[test]
    fn the_secrets_screen_shows_the_antigravity_sign_in() {
        let mut app = app_with(populated());
        app.secrets = Some(crate::sbx::Secrets::default());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("Antigravity sign-in"), "{screen}");
        assert!(screen.contains("not listed by sbx"), "{screen}");

        let mut agy = crate::model::sample("agy-proj", Some("agy"), &[]);
        agy.auth_mode = "none · antigravity".into();
        app.sandboxes.push(agy);
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("not signed in"), "{screen}");
        app.sandboxes.last_mut().unwrap().auth_mode = "oauth · antigravity".into();
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("signed in"), "{screen}");
        assert!(!screen.contains("not signed in"), "{screen}");

        app.secrets = Some(crate::sbx::Secrets {
            services: vec![crate::sbx::Service {
                masked: "(oauth configured)".into(),
                ..crate::sbx::Service::sample(crate::sbx::AGY_OAUTH_SERVICE)
            }],
            custom: Vec::new(),
        });
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("Antigravity sign-in"), "{screen}");
        assert!(screen.contains("(oauth configured)"), "{screen}");
    }

    #[test]
    fn clicking_a_secret_row_opens_it() {
        let mut app = app_with(populated());
        app.secrets = Some(super::tests_support::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        let row = app
            .secret_rows()
            .iter()
            .position(|r| *r == crate::app::SecretRow::Token("private".into()))
            .expect("the token is on the screen");
        click(&mut app, Hit::FormRow(row));
        let Some(Modal::Prompt(prompt)) = &app.modal else {
            panic!("clicking a token asks for a value: {:?}", app.modal)
        };
        assert!(prompt.title().contains("private"), "{}", prompt.title());
    }

    #[test]
    fn the_account_choices_say_which_to_pick_and_what_they_share() {
        use crate::app::AccountChoice;
        let mut app = app_with(populated());

        // OpenCode has no Anthropic account to choose, so none of this is on screen.
        app.open_create();
        let opencode = app
            .catalog
            .backends
            .iter()
            .position(|b| b.kit.name == "opencode-litellm")
            .expect("the OpenCode backend is offered");
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = opencode;
        }
        show_step(&mut app, CreateStep::Harness);
        let gateway = super::tests_support::render(&mut app);
        assert!(!gateway.contains("/login (OAuth)"), "{gateway}");

        let subscription = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account().is_some())
            .expect("a subscription backend is offered");
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = subscription;
        }
        let create = super::tests_support::render(&mut app);
        app.screen = Screen::AuthEdit(super::tests_support::auth_form(AccountChoice::Login));
        let auth = super::tests_support::render(&mut app);
        for screen in [&create, &auth] {
            assert!(screen.contains("recommended"), "{screen}");
            assert!(
                screen.contains("one global login, reused by every sandbox"),
                "{screen}"
            );
            assert!(
                screen.contains("independent Anthropic subscription"),
                "{screen}"
            );
            // The pick list is hidden until the token radio is chosen, so nothing may point at it.
            assert!(!screen.contains("pick one below"), "{screen}");
        }
        // …and on a token, that the name is the account.
        app.screen = Screen::AuthEdit(super::tests_support::auth_form(AccountChoice::Named(
            "work".into(),
        )));
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("uses that one account"), "{screen}");
        assert!(screen.contains("pick one below"), "{screen}");
    }

    #[test]
    fn the_service_secret_command_shows_up_only_while_it_is_missing() {
        let mut app = app_with(populated());
        app.open_create();
        // Said on the Connection step, under the choice that needs it.
        show_step(&mut app, CreateStep::Harness);
        app.secrets = Some(crate::sbx::Secrets::default());

        // The default backend is a LiteLLM one: no service secret to ask for.
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("not stored"), "{screen}");
        assert!(!screen.contains("sbx secret set"), "{screen}");

        // Codex declares no credential, but its base agent owns `openai`, so it is still checked.
        let codex = app
            .catalog
            .backends
            .iter()
            .position(|b| b.kit.name == "codex-openai")
            .expect("the Codex backend is offered");
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = codex;
        }
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("openai is not stored"), "{screen}");
        assert!(screen.contains("sbx secret set openai --oauth"), "{screen}");

        // Selecting a backend that does not need it: the fix is no longer the next thing to do.
        let subscription = app
            .catalog
            .backends
            .iter()
            .position(|b| b.account().is_some())
            .expect("a subscription backend is offered");
        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = subscription;
        }
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("not stored"), "{screen}");
        assert!(!screen.contains("sbx secret set"), "{screen}");

        if let Screen::Create(form) = &mut app.screen {
            form.harness.backend = codex;
        }
        // Stored → neither the marker nor the command.
        app.secrets = Some(crate::sbx::Secrets {
            services: vec![crate::sbx::Service::sample("openai")],
            custom: Vec::new(),
        });
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("not stored"), "{screen}");
        assert!(!screen.contains("sbx secret set"), "{screen}");

        // Not read yet → unknown, which must not be drawn as missing.
        app.secrets = None;
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("not stored"), "{screen}");
        assert!(!screen.contains("sbx secret set"), "{screen}");
    }

    #[test]
    fn renders_the_connection_dialog_and_the_move_form() {
        let mut app = app_with(populated());
        app.secrets = Some(super::tests_support::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        let Screen::Connection(form) = &mut app.screen else {
            panic!("the dialog is open")
        };
        form.fields.name = crate::app::TextInput::new("beta");
        form.fields.url = crate::app::TextInput::new("https://gw.example.org/litellm");
        form.fields.key = crate::app::TextInput::new("not-a-real-key");
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("New LiteLLM connection"), "{screen}");
        assert!(screen.contains("host gw.example.org"), "{screen}");
        assert!(!screen.contains("not-a-real-key"), "{screen}");
        assert!(screen.contains("••••••••••••••"), "{screen}");
        assert!(
            screen.contains("Save") && screen.contains("Cancel"),
            "{screen}"
        );
        render_all_sizes(&mut app);

        app.on_key(KeyEvent::from(KeyCode::Esc));
        let at = app
            .secret_rows()
            .iter()
            .position(|r| *r == crate::app::SecretRow::Connection("acme".into()))
            .unwrap();
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = at;
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("LiteLLM connection — acme"), "{screen}");
        assert!(screen.contains("empty = keep the stored key"), "{screen}");

        app.screen = Screen::List;
        let at = app
            .sandboxes
            .iter()
            .position(|s| s.litellm.is_some())
            .expect("a LiteLLM sandbox among the samples");
        app.cursor = app
            .cells
            .iter()
            .position(|c| *c == crate::app::Cell::Sandbox(at))
            .unwrap_or(at);
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        assert!(
            matches!(app.screen, Screen::ConnSwitch(_)),
            "{:?}",
            app.screen
        );
        let screen = super::tests_support::render(&mut app);
        assert!(
            screen.contains("LiteLLM acme · litellm.example.com"),
            "{screen}"
        );
        assert!(screen.contains("(current)"), "{screen}");
        render_all_sizes(&mut app);
    }

    #[test]
    fn the_connection_forms_click_boxes_sit_on_their_rows_at_80x24() {
        use crate::app::{ConnRow, ConnSwitchRow};
        fn fill(fields: &mut crate::create::NewConnection, key: &str) {
            fields.name = TextInput::new("beta");
            fields.url = TextInput::new(format!("https://gw.example.org/{}", "deep/".repeat(40)));
            fields.key = TextInput::new(key);
        }
        fn label(row: &str) -> &'static str {
            match row {
                "Name" => " name  ",
                "Url" => " URL  ",
                "Key" => " key  ",
                "Conn" => "Connection",
                "Save" => "[ Save ]",
                // A move rebuilds, so its Apply is drawn as a danger: no brackets.
                "Apply" => "  Apply  ",
                _ => "  Cancel  ",
            }
        }
        /// Draw with each row focused in turn; every click box's line shows its own label, and a
        /// click on a text row moves the focus there.
        fn check(app: &mut App, rows: &[String], focus: &dyn Fn(&mut App, usize), key: &str) {
            for at in 0..rows.len() {
                focus(app, at);
                let screen = super::tests_support::render_at(app, 80, 24);
                let lines: Vec<&str> = screen.lines().collect();
                assert!(!screen.contains(key), "the key is drawn: {screen}");
                assert!(
                    app.hits.iter().any(|(_, h)| *h == Hit::FormRow(at)),
                    "{} focused but off screen: {screen}",
                    rows[at]
                );
                for (rect, hit) in app.hits.clone() {
                    let Hit::FormRow(i) = hit else { continue };
                    let line = lines[rect.y as usize];
                    assert!(
                        line.contains(label(&rows[i])),
                        "{} box on {line:?}: {screen}",
                        rows[i]
                    );
                    if matches!(rows[i].as_str(), "Name" | "Url" | "Key") {
                        click_at(app, (rect.x + 2, rect.y));
                        focus_is(app, i, &rows[i]);
                        focus(app, at);
                    }
                }
            }
        }
        fn focus_is(app: &App, want: usize, row: &str) {
            let got = match &app.screen {
                Screen::Connection(form) => form.focus,
                Screen::ConnSwitch(form) => form.focus,
                other => panic!("{row}: the form closed: {other:?}"),
            };
            assert_eq!(got, want, "a click on {row}");
        }
        let key = format!("not-a-real-key-{}", "z".repeat(120));

        let mut app = app_with(populated());
        app.secrets = Some(super::tests_support::sample_secrets());
        app.on_key(KeyEvent::from(KeyCode::Char('S')));
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        let Screen::Connection(form) = &mut app.screen else {
            panic!("the dialog is open")
        };
        fill(&mut form.fields, &key);
        let rows: Vec<String> = form.rows().iter().map(|r| format!("{r:?}")).collect();
        let focus = |app: &mut App, at: usize| {
            if let Screen::Connection(form) = &mut app.screen {
                form.focus = at;
            }
        };
        check(&mut app, &rows, &focus, &key);
        // The caret's end of a long value is what shows while it has focus.
        focus(&mut app, rows.iter().position(|r| r == "Url").unwrap());
        let screen = super::tests_support::render_at(&mut app, 80, 24);
        assert!(
            screen.contains("URL  …") && screen.contains("deep/"),
            "{screen}"
        );
        for row in ["models  ", "default  "] {
            assert!(!screen.contains(row), "no {row:?} row: {screen}");
        }
        assert!(screen.contains("••••"), "{screen}");

        // The same dialog on an existing connection, with sandboxes on it.
        app.on_key(KeyEvent::from(KeyCode::Esc));
        let at = app
            .secret_rows()
            .iter()
            .position(|r| *r == crate::app::SecretRow::Connection("acme".into()))
            .unwrap();
        if let Screen::Secrets(form) = &mut app.screen {
            form.focus = at;
        }
        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Screen::Connection(form) = &mut app.screen else {
            panic!("the edit dialog is open")
        };
        fill(&mut form.fields, &key);
        let rows: Vec<String> = form.rows().iter().map(|r| format!("{r:?}")).collect();
        assert!(!rows.contains(&format!("{:?}", ConnRow::Name)));
        check(&mut app, &rows, &focus, &key);

        // `A` on a LiteLLM sandbox, on *new connection…*.
        app.screen = Screen::List;
        let at = app
            .sandboxes
            .iter()
            .position(|s| s.litellm.is_some())
            .expect("a LiteLLM sandbox among the samples");
        app.cursor = app
            .cells
            .iter()
            .position(|c| *c == crate::app::Cell::Sandbox(at))
            .unwrap_or(at);
        app.on_key(KeyEvent::from(KeyCode::Char('A')));
        let Screen::ConnSwitch(form) = &mut app.screen else {
            panic!("the move form is open: {:?}", app.screen)
        };
        form.pick = crate::litellm::LitellmPick::New;
        fill(&mut form.new_conn, &key);
        let rows: Vec<String> = form.rows().iter().map(|r| format!("{r:?}")).collect();
        assert!(rows.contains(&format!("{:?}", ConnSwitchRow::Key)));
        let focus = |app: &mut App, at: usize| {
            if let Screen::ConnSwitch(form) = &mut app.screen {
                form.focus = at;
            }
        };
        check(&mut app, &rows, &focus, &key);
    }

    #[test]
    fn the_auth_hint_is_only_shown_where_it_leads_somewhere() {
        let subscription = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
        let gateway = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let external = model::sample("extern-proj", None, &[]);
        let mut app = app_with(vec![subscription, gateway, external]);
        let auth = Hit::key(KeyCode::Char('A'));

        drawn(&mut app);
        assert_eq!(
            app.selected().map(|s| s.name.as_str()),
            Some("claude-anthropic-proj")
        );
        assert!(app.hits.iter().any(|(_, h)| *h == auth), "{:?}", app.hits);

        app.on_key(KeyEvent::from(KeyCode::Right));
        drawn(&mut app);
        assert_eq!(
            app.selected().map(|s| s.name.as_str()),
            Some("claude-litellm-proj")
        );
        assert!(app.hits.iter().any(|(_, h)| *h == auth), "{:?}", app.hits);
        assert!(super::tests_support::render(&mut app).contains("A connection"));

        app.on_key(KeyEvent::from(KeyCode::Right));
        drawn(&mut app);
        assert_eq!(app.selected().map(|s| s.name.as_str()), Some("extern-proj"));
        assert!(!app.hits.iter().any(|(_, h)| *h == auth), "{:?}", app.hits);
        // The secrets screen is host-global, so it is offered on every selection, the New row too.
        let secrets = Hit::key(KeyCode::Char('S'));
        assert!(app.hits.iter().any(|(_, h)| *h == secrets));
        app.cursor = app.here;
        drawn(&mut app);
        assert!(
            app.hits.iter().any(|(_, h)| *h == secrets),
            "{:?}",
            app.hits
        );
    }

    #[test]
    fn renders_the_kits_form() {
        let mut app = app_with(populated());
        let addons: Vec<bool> = app
            .catalog
            .addons
            .iter()
            .map(|a| a.kit.name == "rtk")
            .collect();
        let mut wanted = addons.clone();
        if let Some(first) = wanted.first_mut() {
            *first = !*first;
        }
        app.screen = Screen::Kits(KitsForm {
            sandbox: "claude-anthropic-proj".into(),
            original: addons,
            addons: wanted,
            models_offered: false,
            models: Default::default(),
            original_models: Default::default(),
            model_pick: None,
            focus: 0,
            ram_volumes: false,
            history_in_volumes: true,
            harness: None,
        });
        render_all_sizes(&mut app);
    }

    #[test]
    fn renders_the_auth_form() {
        use crate::app::AccountChoice;
        let mut app = app_with(populated());
        for account in [
            AccountChoice::Login,
            AccountChoice::Named("work".into()),
            AccountChoice::New,
        ] {
            app.screen = Screen::AuthEdit(super::tests_support::auth_form(account));
            render_all_sizes(&mut app);
        }
        // A pasted token is never drawn back, at any width.
        app.screen = Screen::AuthEdit(super::tests_support::auth_form(AccountChoice::New));
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("sk-ant"), "{screen}");
        assert!(screen.contains('•'), "{screen}");
        // Picking a token that exists asks for nothing — that is the point of naming them.
        app.screen = Screen::AuthEdit(super::tests_support::auth_form(AccountChoice::Named(
            "work".into(),
        )));
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("nothing to paste"), "{screen}");

        // A real token is longer than the row: while it is typed, the caret stays on screen and
        // the start of the dots gives way behind a `…`.
        let mut form = super::tests_support::auth_form(AccountChoice::New);
        form.token = crate::app::TextInput::new("x".repeat(120));
        form.focus = (form.rows().iter())
            .position(|r| *r == crate::app::AuthRow::Value)
            .unwrap();
        app.screen = Screen::AuthEdit(form);
        let buffer = buffer_at(&mut app, 80, 24);
        let caret = (0..buffer.area.height).find_map(|y| {
            (0..buffer.area.width)
                .find(|&x| {
                    let cell = &buffer[(x, y)];
                    cell.modifier.contains(ratatui::style::Modifier::REVERSED)
                        && line_of(&buffer, y).contains('•')
                })
                .map(|x| (x, y))
        });
        let (x, y) = caret.expect("the caret is drawn");
        assert!(x < 78, "inside the panel: {}", line_of(&buffer, y));
        assert!(
            line_of(&buffer, y).contains("…•"),
            "{}",
            line_of(&buffer, y)
        );
    }

    // --- mouse -----------------------------------------------------------------------------

    use crate::app::{Handoff, Hit};
    use crate::input::{KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use crate::insight::Tab;

    /// Render once so `app.hits` holds this frame's click boxes, at a size where both sections and
    /// every footer hint are on screen: a card scrolled out of view has no click box.
    fn drawn(app: &mut App) {
        let mut terminal = Terminal::new(TestBackend::new(240, 44)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
    }

    fn at(app: &App, want: &Hit) -> (u16, u16) {
        let (rect, _) = app
            .hits
            .iter()
            .find(|(_, hit)| hit == want)
            .unwrap_or_else(|| panic!("no click target for {want:?} in {:?}", app.hits));
        (rect.x + rect.width / 2, rect.y + rect.height / 2)
    }

    fn click_at(app: &mut App, (column, row): (u16, u16)) {
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn click(app: &mut App, want: Hit) {
        drawn(app);
        let point = at(app, &want);
        click_at(app, point);
    }

    fn wheel(app: &mut App, want: Hit, down: bool) {
        drawn(app);
        let (column, row) = at(app, &want);
        app.on_mouse(MouseEvent {
            kind: if down {
                MouseEventKind::ScrollDown
            } else {
                MouseEventKind::ScrollUp
            },
            column,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn a_click_selects_a_card_and_a_second_one_starts_it() {
        let mut app = app_with(populated());
        // Cell 3 is the other-folder card, past the "+ New sandbox" cell that closes section one.
        click(&mut app, Hit::Card(3));
        assert_eq!(app.cursor, 3);
        assert!(app.handoff.is_none(), "one click only selects");

        // That one's workspace is gone, so aim the double-click at one that starts.
        click(&mut app, Hit::Card(0));
        click(&mut app, Hit::Card(0));
        assert_eq!(
            app.handoff,
            Some(Handoff::Agent {
                name: "claude-anthropic-proj".into(),
                // No start folder on the fixture, so this stays sbx's own `run --name` path.
                start: None,
                // A start hands over the label, then updates the agent — see `sbx::label_argv`
                // and `kits::agent_update`.
                prelude: vec![
                    crate::sbx::label_argv("claude-anthropic-proj", "claude-anthropic-proj"),
                    crate::sbx::exec_argv("claude-anthropic-proj", &["claude", "update"]),
                ],
            }),
            "a second click on the selected card starts it"
        );
    }

    #[test]
    fn clicking_the_new_card_opens_the_create_form() {
        let mut app = app_with(populated());
        click(&mut app, Hit::NewCard);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains(&trail(CreateStep::Provider)), "{screen}");
        assert!(screen.contains("Who serves the model"), "{screen}");
        assert!(
            screen.contains("›◉ LiteLLM              Claude Code · OpenCode · Pi"),
            "the cursor opens on the picked provider: {screen}"
        );
    }

    #[test]
    fn clicking_the_other_folders_heading_folds_it_away() {
        let mut app = app_with(populated());
        assert!(app.show_other);
        drawn(&mut app);
        // The footer hint carries the same key, so aim at the heading explicitly: it is the wide
        // one inside the grid, registered before the footer.
        let (rect, _) = app
            .hits
            .iter()
            .find(|(rect, hit)| *hit == Hit::key(KeyCode::Char('a')) && rect.width > 40)
            .expect("the Other folders heading is a click target");
        let point = (rect.x + 2, rect.y);
        click_at(&mut app, point);
        assert!(!app.show_other, "the heading toggles the section it labels");
    }

    #[test]
    fn clicking_a_tab_switches_the_details_pane() {
        let mut app = app_with(populated());
        click(&mut app, Hit::TabAt(Tab::Network.index()));
        assert_eq!(app.tab, Tab::Network);
        click(&mut app, Hit::TabAt(Tab::Overview.index()));
        assert_eq!(app.tab, Tab::Overview);
    }

    #[test]
    fn a_footer_hint_runs_its_key() {
        let mut app = app_with(populated());
        click(&mut app, Hit::key(KeyCode::Char('x')));
        assert_eq!(
            app.handoff,
            Some(Handoff::Shell {
                name: "claude-anthropic-proj".into(),
                workdir: None,
            }),
            "the shell hint opens a shell in the selected sandbox"
        );

        let mut app = app_with(populated());
        click(&mut app, Hit::key(KeyCode::Char('n')));
        let Screen::Create(form) = &app.screen else {
            panic!("n opens the create form")
        };
        assert_eq!(form.wizard_step(), CreateStep::Provider);
    }

    #[test]
    fn the_prune_hint_is_only_offered_while_something_is_stopped() {
        let mut app = app_with(populated());
        let screen = tests_support::render_at(&mut app, 240, 45);
        assert!(
            screen.contains("P prune"),
            "two of the three sandboxes are stopped: {screen}"
        );

        // On the New card the whole footer is different; the host-wide keys still belong there.
        app.on_key(KeyEvent::from(KeyCode::Home));
        app.on_key(KeyEvent::from(KeyCode::Left)); // wraps to the last cell…
        app.on_key(KeyEvent::from(KeyCode::Left)); // …and back onto + New sandbox
        assert!(app.on_new_card());
        let screen = tests_support::render_at(&mut app, 240, 45);
        assert!(screen.contains("P prune"), "{screen}");

        // Without `sbx prune --dry-run` under test, the confirmation lists the stopped sandboxes.
        let mut app = app_with(populated());
        app.on_key(KeyEvent::from(KeyCode::Char('P')));
        let Some(Modal::Confirm(confirm)) = &app.modal else {
            panic!("P opens a confirmation: {:?}", app.modal)
        };
        assert!(confirm.danger, "it removes containers");
        let body = confirm.body.join("\n");
        assert!(
            body.contains("opencode-proj") && body.contains("ts-lab"),
            "{body}"
        );
        assert!(
            !body.contains("claude-anthropic-proj"),
            "the running one is never a candidate: {body}"
        );
        assert!(body.contains("Workspace files are untouched"), "{body}");
        assert_eq!(confirm.job.steps.len(), 1);
        assert!(
            confirm.job.steps[0].argv.contains(&"--force".to_string()),
            "no terminal to answer a prompt"
        );
        let screen = tests_support::render_at(&mut app, 140, 45);
        assert!(screen.contains("Prune stopped sandboxes"), "{screen}");

        // Everything running → nothing to prune, and the key goes away.
        let mut all_running = populated();
        for sb in &mut all_running {
            sb.running = true;
        }
        let mut app = app_with(all_running);
        assert!(!app.can_prune());
        let screen = tests_support::render_at(&mut app, 240, 45);
        assert!(!screen.contains("P prune"), "{screen}");
        // …and pressing it anyway says so instead of opening an empty confirmation.
        app.on_key(KeyEvent::from(KeyCode::Char('P')));
        assert!(
            app.modal.is_none(),
            "no confirmation with nothing to confirm"
        );
        assert!(app
            .status
            .as_ref()
            .is_some_and(|(m, _, _)| m.contains("Nothing to prune")));
    }

    #[test]
    fn the_wheel_scrolls_the_pane_it_is_over() {
        let mut app = app_with(populated());
        app.select_tab(Tab::Network.index());
        wheel(&mut app, Hit::ScrollDetail, true);
        assert!(app.detail_scroll > 0);
        wheel(&mut app, Hit::ScrollDetail, false);
        assert_eq!(app.detail_scroll, 0);

        // Over the list it moves the selection instead, one row at a time.
        assert_eq!(app.cursor, 0);
        wheel(&mut app, Hit::ScrollCards, true);
        assert_eq!(app.cursor, 1);
        wheel(&mut app, Hit::ScrollCards, false);
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn an_open_modal_swallows_every_click_underneath_it() {
        let mut app = app_with(populated());
        drawn(&mut app);
        let footer = at(&app, &Hit::key(KeyCode::Char('n')));

        app.modal = Some(Modal::Help);
        drawn(&mut app);
        click_at(&mut app, footer);
        // Without the guard, `n` would have reached the modal handler and opened the form.
        assert!(matches!(app.screen, Screen::List));
    }

    #[test]
    fn the_footer_names_the_open_modals_keys() {
        let mut app = app_with(populated());
        app.cursor = 0;
        app.on_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(matches!(app.modal, Some(Modal::Confirm(_))));
        let screen = super::tests_support::render(&mut app);
        let footer = screen.lines().last().unwrap_or_default();
        assert!(
            footer.contains("esc cancel") && footer.contains("y confirm"),
            "{footer}"
        );
        assert!(!footer.contains("shell"), "the list's keys: {footer}");

        drawn(&mut app);
        let (rect, _) = app
            .hits
            .iter()
            .find(|(rect, hit)| rect.y == 43 && *hit == Hit::key(KeyCode::Esc))
            .expect("the footer's esc is a button");
        let point = (rect.x + rect.width / 2, rect.y);
        click_at(&mut app, point);
        assert!(app.modal.is_none(), "cancelled, nothing deleted");
    }

    #[test]
    fn the_footer_names_a_prompts_answer_by_its_verb() {
        let mut app = app_with(populated());
        app.cursor = 0;
        for (key, verb) in [('r', "↵ rename"), ('p', "↵ publish"), ('c', "↵ check")] {
            app.modal = None;
            app.on_key(KeyEvent::from(KeyCode::Char(key)));
            assert!(matches!(app.modal, Some(Modal::Prompt(_))), "{key}");
            let screen = super::tests_support::render(&mut app);
            let footer = screen.lines().last().unwrap_or_default();
            assert!(
                footer.contains(verb) && footer.contains("esc cancel"),
                "{footer}"
            );
            assert!(!footer.contains("↵ run"), "{footer}");
        }
    }

    #[test]
    fn clicking_a_form_row_toggles_it() {
        let mut app = app_with(populated());
        app.open_create();
        let rtk = app
            .catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "rtk")
            .expect("the rtk kit is part of this repo");
        // Ask the step where that row landed rather than counting: the host caches come first,
        // and how many there are is the checkout's business.
        show_step(&mut app, CreateStep::Kits);
        let row = wizard_index(&app, WizardRow::Field(CreateRow::Addon(rtk)));
        click(&mut app, Hit::FormRow(row));
        let Screen::Create(form) = &app.screen else {
            panic!("still on the create form")
        };
        assert!(form.kits.addons[rtk], "clicking a kit row ticks it");
    }

    #[test]
    #[cfg(unix)]
    fn host_caches_this_host_lacks_are_greyed_and_cannot_be_ticked() {
        let base = std::env::temp_dir().join(format!("aisbx-form-detect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        for dir in [".npm/_cacache", ".m2/repository"] {
            std::fs::create_dir_all(home.join(dir)).unwrap();
        }
        let mut app = app_with(populated());
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let index = |name: &str| {
            catalog
                .addons
                .iter()
                .position(|a| a.kit.name == name)
                .unwrap_or_else(|| panic!("the create form offers {name}"))
        };
        if let Screen::Create(form) = &mut app.screen {
            form.kits.host = catalog
                .addons
                .iter()
                .map(|a| model::host_cache_detection_in(&a.kit.name, Some(home.as_path())))
                .collect();
        }
        let ticked = |app: &App, i: usize| match &app.screen {
            Screen::Create(form) => form.kits.addons[i],
            _ => unreachable!("the create form is open"),
        };
        let focus = |app: &mut App, i: usize| focus_row(app, CreateRow::Addon(i));

        for name in ["host-gradle", "host-pip", "host-skills"] {
            let i = index(name);
            let row = focus(&mut app, i);
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
            assert!(!ticked(&app, i), "space leaves {name} unticked");
            app.on_key(KeyEvent::from(KeyCode::Enter));
            assert!(!ticked(&app, i), "↵ leaves {name} unticked");
            click(&mut app, Hit::FormRow(row));
            assert!(!ticked(&app, i), "a click leaves {name} unticked");
        }
        for name in ["host-npm", "host-maven"] {
            let i = index(name);
            focus(&mut app, i);
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
            assert!(ticked(&app, i), "space ticks {name}");
            app.on_key(KeyEvent::from(KeyCode::Char(' ')));
            assert!(!ticked(&app, i), "and unticks it");
        }

        focus(&mut app, index("host-gradle"));
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("Host caches"), "{screen}");
        assert!(screen.contains("Additional kits"), "{screen}");
        assert!(
            screen.find("Host caches") < screen.find("Additional kits"),
            "the host caches come first: {screen}"
        );
        assert!(screen.contains("[-] host-gradle"), "greyed: {screen}");
        assert!(screen.contains("no ~/.gradle/caches"), "{screen}");
        assert!(strip_text(&screen).contains("cannot be ticked"), "{screen}");
        assert!(screen.contains("[ ] host-maven"), "tickable: {screen}");
        assert!(
            screen.contains("· found "),
            "the folder found is named: {screen}"
        );
        assert!(screen.contains("[-] host-skills"), "{screen}");
        assert!(screen.contains("[-] host-pip"), "{screen}");
        assert!(screen.contains("[ ] host-npm"), "{screen}");
        let skills_note = match &app.screen {
            Screen::Create(form) => form.kits.host[index("host-skills")]
                .as_ref()
                .unwrap()
                .reason("host-skills"),
            _ => unreachable!(),
        };
        assert!(
            skills_note.contains("skills from the sbx store are still shared"),
            "{skills_note}"
        );

        // The skills-mode row does not depend on any of it.
        focus_row(&mut app, CreateRow::Skills);
        let mut seen = Vec::new();
        for _ in 0..3 {
            app.on_key(KeyEvent::from(KeyCode::Right));
            if let Screen::Create(form) = &app.screen {
                seen.push(form.workspace.skills);
            }
        }
        seen.sort_by_key(|m| m.flag());
        assert_eq!(
            seen,
            [
                crate::sbx::SkillsMode::Off,
                crate::sbx::SkillsMode::Readonly,
                crate::sbx::SkillsMode::Readwrite
            ]
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_account_rows_belong_to_the_subscription_backend_only() {
        let mut app = app_with(populated());
        app.open_create();
        let rows = |app: &App| {
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            form.rows(&app.catalog)
        };
        let provider = |app: &App, p: crate::kits::Provider| {
            let Screen::Create(form) = &app.screen else {
                panic!("the create form is open")
            };
            let at = form.providers(&app.catalog).iter().position(|o| *o == p);
            WizardRow::Provider(at.expect("offered"))
        };
        assert!(!rows(&app).contains(&CreateRow::AuthOauth));
        show_step(&mut app, CreateStep::Harness);
        assert!(
            !wizard_rows(&app)
                .iter()
                .any(|r| matches!(r, WizardRow::Connection(_))),
            "no account radio on the gateway"
        );

        // Anthropic picks the subscription backend, whose step offers /login and setup token.
        show_step(&mut app, CreateStep::Provider);
        let anthropic = provider(&app, crate::kits::Provider::Anthropic);
        let at = wizard_index(&app, anthropic);
        click(&mut app, Hit::FormRow(at));
        show_step(&mut app, CreateStep::Harness);
        let login = wizard_index(&app, WizardRow::Connection(ConnChoice::Login));
        click(&mut app, Hit::FormRow(login));
        assert!(rows(&app).contains(&CreateRow::AuthOauth));
        // The token fields stay hidden until the token option is picked.
        assert!(!rows(&app).contains(&CreateRow::Token));

        show_step(&mut app, CreateStep::Harness);
        let token = wizard_index(&app, WizardRow::Connection(ConnChoice::Token));
        click(&mut app, Hit::FormRow(token));
        assert!(rows(&app).contains(&CreateRow::TokenName));
        assert!(rows(&app).contains(&CreateRow::Token));

        show_step(&mut app, CreateStep::Provider);
        let litellm = provider(&app, crate::kits::Provider::Litellm);
        let at = wizard_index(&app, litellm);
        click(&mut app, Hit::FormRow(at));
        let rows = rows(&app);
        assert!(!rows.contains(&CreateRow::AuthOauth), "{rows:?}");
        assert!(!rows.contains(&CreateRow::Token), "{rows:?}");
    }

    #[test]
    fn the_list_and_the_overview_keep_the_mount_and_the_start_folder_apart() {
        // Left on the fixture's own `/proj`, which is the cwd these tests run in — a sandbox
        // whose workspace is elsewhere sits under "Other folders" and is not the selected row.
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = app_with(vec![sb.clone()]);
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("/proj"), "{screen}");
        assert!(
            !screen.contains("opens in"),
            "a sandbox with no start folder has one answer and does not say it twice: {screen}"
        );

        sb.start_dir = Some(std::path::PathBuf::from("notes"));
        let mut app = app_with(vec![sb.clone()]);
        let screen = super::tests_support::render(&mut app);
        assert!(
            screen.contains("This folder  /proj") && screen.contains("› notes"),
            "the heading carries the mount, the row the start folder: {screen}"
        );
        assert!(screen.contains("opens in"), "{screen}");
        // The mount is still stated in full, unqualified — narrowing where the agent opens must
        // never read as narrowing what the sandbox can see.
        assert!(
            screen.contains("folder") && screen.contains("this folder"),
            "{screen}"
        );

        // A folder deleted since it was chosen is called out rather than silently ignored: the
        // next start will open somewhere other than the row promised.
        sb.start_dir = Some(std::path::PathBuf::from("gone-away"));
        let mut app = app_with(vec![sb]);
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("gone"), "{screen}");
    }

    #[test]
    fn the_overview_names_the_models_a_sandbox_pinned() {
        use crate::models::{Overrides, Slot};
        let mut sb = model::sample("claude-litellm-proj", Some("claude-litellm"), &[]);
        let mut app = app_with(vec![sb.clone()]);
        let model = app
            .catalog
            .models
            .first()
            .expect("the gateway has models")
            .clone();

        assert!(
            !super::tests_support::render(&mut app).contains("models"),
            "an unpinned sandbox says nothing"
        );

        let mut pinned = Overrides::default();
        pinned.set(Slot::Haiku, Some(model.id.clone()));
        sb.models = pinned;
        let mut app = app_with(vec![sb.clone()]);
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("models"), "{screen}");
        assert!(screen.contains("haiku"), "{screen}");
        assert!(screen.contains(&model.name), "{screen}");

        // The same pins on a backend that cannot use them are not reported as if they applied —
        // a subscription sandbox reaches api.anthropic.com, where these ids mean nothing.
        sb.backend = Some("claude-anthropic".into());
        let mut app = app_with(vec![sb]);
        assert!(!super::tests_support::render(&mut app).contains(&model.name));
    }

    #[test]
    fn the_overview_says_when_the_volumes_are_in_ram() {
        let mut sb = model::sample("claude-anthropic-proj", Some("claude-anthropic"), &[]);
        let mut app = app_with(vec![sb.clone()]);
        assert!(!super::tests_support::render(&mut app).contains("in RAM (tmpfs)"));
        sb.tmpfs_volumes = true;
        let mut app = app_with(vec![sb]);
        let screen = super::tests_support::render(&mut app);
        assert!(
            screen.contains("volumes") && screen.contains("in RAM (tmpfs)"),
            "{screen}"
        );
    }

    #[test]
    fn the_model_list_title_says_where_the_list_came_from() {
        let mut app = app_with(Vec::new());
        app.open_create();
        show_step(&mut app, CreateStep::Models);
        let screen = super::tests_support::render(&mut app);
        // The refresh row says it; the heading under it does not say it again.
        assert!(screen.contains("Refresh the list   gateway, "), "{screen}");
        assert_eq!(screen.matches("gateway, ").count(), 1, "{screen}");
        assert!(screen.contains("leaves it to the agent"), "{screen}");

        let catalog = std::sync::Arc::make_mut(&mut app.catalog);
        catalog.models_source = crate::models::Source::Gateway {
            sandbox: "claude-litellm-proj".into(),
            fetched_at: crate::state::now_epoch().saturating_sub(3 * 3_600) as i64,
        };
        let screen = super::tests_support::render(&mut app);
        assert!(
            screen.contains("Refresh the list   gateway, 3 hours ago"),
            "{screen}"
        );
        render_all_sizes(&mut app);
    }

    #[test]
    fn the_model_section_says_what_each_alias_resolves_to() {
        use crate::models::Slot;
        let mut app = app_with(Vec::new());
        app.open_create();
        show_step(&mut app, CreateStep::Models);

        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains("Starts on"), "{screen}");
        assert!(screen.contains("opus"), "{screen}");
        // An unpinned alias reads as what it is, and the heading says what that means.
        assert!(screen.contains("(default)"), "{screen}");
        assert!(screen.contains("leaves it to the agent"), "{screen}");
        // Each model is listed once, as offered, until a slot's list is opened.
        let first = app
            .catalog
            .models
            .first()
            .expect("the gateway has models")
            .clone();
        assert_eq!(screen.matches(&first.id).count(), 1, "{screen}");

        let row = |app: &App, want: CreateRow| wizard_index(app, WizardRow::Field(want));
        let open = row(&app, CreateRow::Model(Slot::Sonnet));
        click(&mut app, Hit::FormRow(open));
        let screen = super::tests_support::render(&mut app);
        // Both halves: the gateway's own name for the model, and the id that goes in the variable.
        assert!(screen.contains(&first.name), "{screen}");
        assert!(
            screen.matches(&first.id).count() > 1,
            "listed under the slot: {screen}"
        );
        render_all_sizes(&mut app);

        // Clicking a model pins it — and the row now says so with the list closed again.
        let pick = row(&app, CreateRow::ModelPick(Slot::Sonnet, 0));
        click(&mut app, Hit::FormRow(pick));
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        assert_eq!(
            form.connection.models.get(Slot::Sonnet),
            Some(first.id.as_str())
        );
        let screen = super::tests_support::render(&mut app);
        assert!(screen.contains(&first.name), "{screen}");
    }

    #[test]
    fn arrow_keys_move_by_a_row_and_wrap_only_sideways() {
        let mut app = app_with(populated());
        drawn(&mut app);

        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.cursor, 1);
        app.on_key(KeyEvent::from(KeyCode::Up));
        app.on_key(KeyEvent::from(KeyCode::Up));
        assert_eq!(app.cursor, 0, "up stops at the first row");
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.cursor, 1, "j is the vim down");
        // `h` is the vim left, not the help modal.
        app.on_key(KeyEvent::from(KeyCode::Char('h')));
        assert_eq!(app.cursor, 0);
        assert!(app.modal.is_none(), "h moves, it does not open help");
        app.on_key(KeyEvent::from(KeyCode::End));
        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(app.cursor, app.slots() - 1, "down stops at the last row");
        app.on_key(KeyEvent::from(KeyCode::Right));
        assert_eq!(app.cursor, 0, "right wraps to the first row");
        app.on_key(KeyEvent::from(KeyCode::Left));
        assert_eq!(
            app.cursor,
            app.slots() - 1,
            "wrapping backwards lands on the last row of the list, section two included"
        );
    }

    #[test]
    fn the_new_card_is_reachable_and_enter_opens_the_form() {
        let mut app = app_with(populated());
        drawn(&mut app);

        app.on_key(KeyEvent::from(KeyCode::Home));
        app.on_key(KeyEvent::from(KeyCode::Left)); // wraps to the last cell…
        app.on_key(KeyEvent::from(KeyCode::Left)); // …and back into section one
        assert_eq!(app.cursor, app.here);
        assert!(app.on_new_card());
        assert!(app.selected().is_none(), "no sandbox is selected there");

        app.on_key(KeyEvent::from(KeyCode::Enter));
        let Screen::Create(form) = &app.screen else {
            panic!("↵ on + New sandbox creates rather than erroring about no selection")
        };
        assert_eq!(
            form.wizard_step(),
            CreateStep::Provider,
            "on the wizard's first step"
        );
    }

    #[test]
    fn a_folder_without_sandboxes_still_lists_the_others() {
        let mut app = app_with(populated());
        app.cwd = std::path::PathBuf::from("/somewhere-else");
        app.refresh_cells();

        assert_eq!(app.here, 0);
        assert_eq!(app.elsewhere, 3);
        assert_eq!(app.cursor, 0);
        assert!(
            app.on_new_card(),
            "the empty section still has a cell to sit on"
        );
        assert_eq!(app.slots(), 4, "the New card plus all three sandboxes");

        // Collapsing leaves only the New card reachable, and the count survives so the heading
        // can say what folding it away hid.
        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.slots(), 1);
        assert_eq!(app.elsewhere, 3);
        drawn(&mut app);
    }

    #[test]
    fn a_shows_and_hides_the_other_folders_section() {
        let mut app = app_with(populated());
        assert_eq!(app.slots(), 4, "two here, the New card, one elsewhere");

        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(!app.show_other);
        assert_eq!(app.slots(), 3, "the other folder's card is folded away");
        assert!(app.cursor < app.slots());

        app.on_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(app.show_other);
        assert_eq!(app.slots(), 4);
        assert_eq!(
            app.cursor,
            app.here + 1,
            "expanding puts the cursor on what it just revealed, which can be below the fold"
        );
    }

    #[test]
    fn renders_every_modal() {
        let mut app = app_with(populated());

        app.modal = Some(Modal::Help);
        render_all_sizes(&mut app);

        app.modal = Some(Modal::Confirm(Confirm {
            title: "Delete sandbox".into(),
            body: vec![
                "My private Claude Code".into(),
                String::new(),
                "gone forever".into(),
            ],
            danger: true,
            confirm_label: "Delete".into(),
            yes: true,
            job: PendingJob {
                title: "t".into(),
                steps: Vec::new(),
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        }));
        render_all_sizes(&mut app);

        for (ask, value) in [
            (Ask::Rename, "My private Claude Code"),
            (Ask::PublishPort, "8080:3000"),
            (Ask::UnpublishPort, "8080:3000"),
            (Ask::CheckHost, "services.gradle.org"),
            (
                Ask::SetSecret(crate::app::SecretTarget::Token("private".into())),
                "sk-ant-oat01-0123456789",
            ),
        ] {
            app.modal = Some(Modal::Prompt(Prompt {
                help: vec!["Generate one with claude setup-token.".into()],
                ask,
                subject: "claude-anthropic-proj".into(),
                input: TextInput::new(value),
            }));
            render_all_sizes(&mut app);
        }
        // The masked prompt must never draw the value back, at any width.
        let screen = super::tests_support::render(&mut app);
        assert!(!screen.contains("sk-ant"), "{screen}");
        assert!(screen.contains('•'), "{screen}");

        app.modal = Some(Modal::Running(
            Job::spawn("Creating", Vec::new()),
            PendingJob {
                title: "Creating".into(),
                steps: Vec::new(),
                post: PostJob::default(),
                fallback: None,
                fallback_hint: None,
            },
        ));
        render_all_sizes(&mut app);
    }

    // --- the create wizard ------------------------------------------------------------------------
    fn litellm_connection(name: &str) -> crate::litellm::Connection {
        crate::litellm::Connection {
            name: name.into(),
            url: "https://litellm.example.com".into(),
        }
    }

    #[test]
    fn the_connection_row_shows_the_last_used_connection() {
        for (width, height) in [(110, 38), (110, 40)] {
            let mut app = app_with(populated());
            open_wizard(&mut app, CreateStep::Connection);
            if let Screen::Create(form) = &mut app.screen {
                assert_eq!(
                    app.catalog.backends[form.harness.backend].kit.name,
                    "claude-litellm"
                );
                form.connection.connections =
                    vec![litellm_connection("acme"), litellm_connection("beta")];
                form.connection.litellm = Some("beta".into());
                form.connection.last_litellm = Some("beta".into());
            }
            assert!(
                wizard_rows(&app).contains(&WizardRow::Field(CreateRow::LitellmConn)),
                "{:?}",
                wizard_rows(&app)
            );
            let screen = super::tests_support::render_at(&mut app, width, height);
            assert!(
                screen.contains("‹ beta · litellm.example.com ›"),
                "{width}×{height}: {screen}"
            );
            assert!(screen.contains("(last used)"), "{width}×{height}: {screen}");
            for label in ["[ Next ]", "  Back  ", "  Cancel  "] {
                assert!(
                    screen.contains(label),
                    "{width}×{height}: {label}: {screen}"
                );
            }

            // Another connection picked: named, and no longer marked as the last used one.
            if let Screen::Create(form) = &mut app.screen {
                form.connection.litellm = Some("acme".into());
            }
            let screen = super::tests_support::render_at(&mut app, width, height);
            assert!(
                screen.contains("‹ acme · litellm.example.com ›"),
                "{width}×{height}: {screen}"
            );
            assert!(
                !screen.contains("(last used)"),
                "{width}×{height}: {screen}"
            );
        }
    }

    #[test]
    fn the_connection_row_marks_a_missing_key_and_offers_a_new_connection() {
        let mut app = app_with(populated());
        open_wizard(&mut app, CreateStep::Connection);
        if let Screen::Create(form) = &mut app.screen {
            form.connection.connections = vec![litellm_connection("acme")];
            form.connection.litellm = Some("acme".into());
            form.connection.last_litellm = None;
        }
        app.secrets = None;
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(!screen.contains("key missing"), "{screen}");

        app.secrets = Some(crate::sbx::Secrets::default());
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("key missing"), "{screen}");

        app.secrets = Some(crate::sbx::Secrets {
            services: Vec::new(),
            custom: vec![crate::sbx::CustomSecret {
                scope: crate::sbx::GLOBAL_SCOPE.into(),
                targets: vec!["litellm.example.com".into()],
                env: String::new(),
                placeholder: crate::litellm::placeholder("acme"),
                masked: "***".into(),
            }],
        });
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(!screen.contains("key missing"), "{screen}");

        if let Screen::Create(form) = &mut app.screen {
            form.connection.connections = Vec::new();
            form.connection.litellm = None;
            form.connection.new_conn.key = crate::app::TextInput::new("not-a-real-key");
        }
        for height in [38, 40] {
            let screen = super::tests_support::render_at(&mut app, 110, height);
            assert!(screen.contains("‹ new connection… ›"), "{height}: {screen}");
            for hint in ["(a–z, 0–9, -)", "(https://…)"] {
                assert!(screen.contains(hint), "{height}: {hint}: {screen}");
            }
            assert!(
                !screen.contains("not-a-real-key"),
                "the key is masked: {screen}"
            );
            assert!(screen.contains("[ Next ]"), "{height}: {screen}");
        }
    }

    /// The wizard's trail as drawn on `step` in full: every step numbered, the ones behind ticked,
    /// the current one as a chip (reversed, with a space of padding each side).
    fn trail(step: CreateStep) -> String {
        CreateStep::ALL
            .iter()
            .map(|s| match s.number().cmp(&step.number()) {
                std::cmp::Ordering::Less => format!("✓ {} {}", s.number(), s.title()),
                std::cmp::Ordering::Equal => format!(" {} {} ", s.number(), s.title()),
                std::cmp::Ordering::Greater => format!("{} {}", s.number(), s.title()),
            })
            .collect::<Vec<_>>()
            .join("  ")
    }

    /// The wizard's help strip as one line of text: the lines under its rule (`─ <title> ─…`) up
    /// to the button line, inside the panel's borders.
    /// The help strip's rule: the last rule over the panel's rows that is not the ledger's.
    fn is_strip_rule(line: &str) -> bool {
        line.starts_with("  ─ ") && !line.starts_with("  ─ so far ")
    }

    fn strip_text(screen: &str) -> String {
        let lines: Vec<&str> = screen.lines().map(|l| l.trim_matches('"')).collect();
        let Some(rule) = lines.iter().rposition(|l| is_strip_rule(l)) else {
            return String::new();
        };
        lines[rule + 1..]
            .iter()
            .take_while(|l| !l.contains("[ ") && !l.starts_with("  ─ "))
            .map(|l| l.trim_matches('│').trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Open the wizard as `n` does, and jump to `step`.
    fn open_wizard(app: &mut App, step: CreateStep) {
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        let catalog = std::sync::Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the wizard is open")
        };
        form.wizard_goto(&catalog, step)
            .expect("the default answers pass");
    }

    fn wizard_rows(app: &App) -> Vec<WizardRow> {
        match &app.screen {
            Screen::Create(form) => form.wizard_rows(&app.catalog),
            _ => panic!("the wizard is open"),
        }
    }

    #[test]
    fn every_wizard_step_shows_its_trail_heading_and_buttons_at_110_by_40() {
        let mut cases: Vec<(&str, App)> = Vec::new();
        for step in CreateStep::ALL {
            let mut app = app_with(Vec::new());
            app.mcp = (0..3)
                .map(|i| crate::sbx::McpServer {
                    name: format!("server-{i}"),
                    status: "ready".into(),
                    ..crate::sbx::McpServer::default()
                })
                .collect();
            open_wizard(&mut app, step);
            cases.push((step.title(), app));
        }
        let mut gateway = app_with(Vec::new());
        gateway.secrets = Some(crate::sbx::Secrets::default());
        open_wizard(&mut gateway, CreateStep::Harness);
        cases.push(("gateway, token missing", gateway));
        // *new connection…*: its four fields under the Connection row.
        let mut new_conn = app_with(Vec::new());
        open_wizard(&mut new_conn, CreateStep::Harness);
        if let Screen::Create(form) = &mut new_conn.screen {
            form.connection.litellm = None;
        }
        cases.push(("gateway, new connection", new_conn));
        let mut token = app_with(Vec::new());
        token.secrets = Some(crate::sbx::Secrets {
            services: vec![crate::sbx::Service::sample("litellm")],
            custom: ["private", "work"]
                .iter()
                .map(|name| crate::sbx::CustomSecret {
                    scope: crate::sbx::GLOBAL_SCOPE.into(),
                    targets: vec![crate::sbx::TOKEN_HOST.into()],
                    env: String::new(),
                    placeholder: crate::sbx::named_token_placeholder(name),
                    masked: "***".into(),
                })
                .collect(),
        });
        open_wizard(&mut token, CreateStep::Harness);
        let catalog = std::sync::Arc::clone(&token.catalog);
        if let Screen::Create(form) = &mut token.screen {
            form.pick_connection(&catalog, ConnChoice::Token);
            assert_eq!(form.connection.tokens.len(), 2);
        }
        cases.push(("setup token, two stored", token));

        for (case, mut app) in cases {
            let step = match &app.screen {
                Screen::Create(form) => form.wizard_step(),
                _ => panic!("the wizard is open"),
            };
            let screen = super::tests_support::render_at(&mut app, 110, 40);
            // The panel names the sandbox being built; the trail says where the wizard is, and
            // the body opens with core's line for the step, not with its title again.
            let (title, heading) = match &app.screen {
                Screen::Create(form) => (
                    format!("New sandbox {} in /proj", form.sbx_name(&app)),
                    form.step_heading(step, &app.catalog).0,
                ),
                _ => panic!("the wizard is open"),
            };
            assert!(screen.contains(&title), "{case}: {screen}");
            assert!(screen.contains(&trail(step)), "{case}: {screen}");
            assert!(screen.contains(&heading), "{case}: {screen}");
            assert!(
                !screen.contains("Step "),
                "{case}: no second header: {screen}"
            );
            // The step's main action first, in brackets; the others plain.
            let labels: &[&str] = match step {
                CreateStep::Review => &[
                    "[ Create & start ]",
                    "  Create only  ",
                    "  Back  ",
                    "  Cancel  ",
                ],
                _ => &["[ Next ]", "  Back  ", "  Cancel  "],
            };
            for label in labels {
                assert!(screen.contains(label), "{case}: {label}: {screen}");
            }
            // Every row and button is a click target — so none was scrolled away — and only the
            // drawn buttons are buttons.
            let rows = wizard_rows(&app);
            let buttons = rows
                .iter()
                .filter(|row| {
                    matches!(
                        row,
                        WizardRow::Back
                            | WizardRow::Next
                            | WizardRow::CreateStart
                            | WizardRow::CreateOnly
                            | WizardRow::Cancel
                    )
                })
                .count();
            assert_eq!(buttons, labels.len(), "{case}");
            for (i, row) in rows.iter().enumerate() {
                assert!(
                    app.hits.iter().any(|(_, hit)| *hit == Hit::FormRow(i)),
                    "{case}: {row:?} is on screen and clickable: {screen}"
                );
            }
            for s in CreateStep::ALL {
                assert!(
                    app.hits.iter().any(|(_, hit)| *hit == Hit::WizardStep(s)),
                    "{case}: the step {s:?} of the trail is clickable"
                );
            }
        }
    }

    #[test]
    fn the_wizard_trail_and_buttons_are_clickable() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        click(&mut app, Hit::WizardStep(CreateStep::Kits));
        let Screen::Create(form) = &app.screen else {
            panic!("the wizard is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Kits);
        let next = wizard_rows(&app)
            .iter()
            .position(|r| *r == WizardRow::Next)
            .unwrap();
        click(&mut app, Hit::FormRow(next));
        let Screen::Create(form) = &app.screen else {
            panic!("the wizard is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Workspace);
    }

    #[test]
    fn the_wizard_workspace_step_scrolls_to_the_focused_row() {
        let mut app = app_with(Vec::new());
        app.mcp = (0..30)
            .map(|i| crate::sbx::McpServer {
                name: format!("server-{i:02}"),
                status: "ready".into(),
                ..crate::sbx::McpServer::default()
            })
            .collect();
        open_wizard(&mut app, CreateStep::Workspace);
        let last = WizardRow::Field(CreateRow::Mcp(29));
        let at = wizard_rows(&app).iter().position(|r| *r == last).unwrap();
        for _ in 0..at {
            app.on_key(KeyEvent::from(KeyCode::Down));
        }
        let screen = super::tests_support::render_at(&mut app, 110, 34);
        assert!(screen.contains("server-29"), "{screen}");
        assert!(!screen.contains("server-00"), "scrolled: {screen}");
        assert!(screen.contains(&trail(CreateStep::Workspace)), "{screen}");
        assert!(screen.contains("[ Next ]"), "{screen}");

        let rows = wizard_rows(&app);
        for (at, row) in rows.iter().enumerate() {
            if let Screen::Create(form) = &mut app.screen {
                form.set_wizard_focus(at);
            }
            let screen = super::tests_support::render_at(&mut app, 110, 34);
            assert!(
                screen.contains(&trail(CreateStep::Workspace)),
                "{row:?}: {screen}"
            );
            assert!(screen.contains("[ Next ]"), "{row:?}: {screen}");
            assert!(
                app.hits.iter().any(|(_, hit)| *hit == Hit::FormRow(at)),
                "{row:?} is on screen: {screen}"
            );
        }
    }

    #[test]
    fn the_wizard_greys_missing_host_caches_and_keeps_the_skills_row() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Kits);
        let catalog = std::sync::Arc::clone(&app.catalog);
        let nowhere = std::path::Path::new("/nonexistent-aisbx-wizard-home");
        if let Screen::Create(form) = &mut app.screen {
            form.kits.host = catalog
                .addons
                .iter()
                .map(|a| model::host_cache_detection_in(&a.kit.name, Some(nowhere)))
                .collect();
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("Host caches"), "{screen}");
        assert!(screen.contains("[-] host-gradle"), "greyed: {screen}");
        assert!(screen.contains("[-] host-skills"), "greyed: {screen}");

        if let Screen::Create(form) = &mut app.screen {
            form.wizard_goto(&catalog, CreateStep::Workspace).unwrap();
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("share skills"), "{screen}");
        assert!(
            wizard_rows(&app).contains(&WizardRow::Field(CreateRow::Skills)),
            "the skills row is offered"
        );
    }

    #[test]
    fn host_cache_detection_never_touches_the_skills_row() {
        use crate::app::AccountChoice;
        let found = model::HostDetect {
            mounts: vec!["/host/folder:ro".into()],
            ..model::HostDetect::default()
        };
        let catalog = std::sync::Arc::clone(&app_with(Vec::new()).catalog);
        let mut answers: Vec<(usize, AccountChoice)> = catalog
            .offered_backends()
            .into_iter()
            .map(|b| (b, AccountChoice::Login))
            .collect();
        let login = catalog
            .backends
            .iter()
            .position(|b| b.account().is_some())
            .expect("a subscription backend is offered");
        answers.push((login, AccountChoice::New));
        for (backend, account) in answers {
            for state in ["all found", "none found", "skills missing"] {
                let mut app = app_with(Vec::new());
                app.open_create();
                let Screen::Create(form) = &mut app.screen else {
                    unreachable!("the create form is open")
                };
                form.harness.backend = backend;
                form.connection.account = account.clone();
                for (i, addon) in catalog.addons.iter().enumerate() {
                    if form.kits.host[i].is_none() {
                        continue;
                    }
                    let detected = match state {
                        "all found" => true,
                        "none found" => false,
                        _ => addon.kit.name != crate::kits::HOST_SKILLS,
                    };
                    form.kits.host[i] = Some(match detected {
                        true => found.clone(),
                        false => model::HostDetect::default(),
                    });
                }
                form.wizard_set_step(&catalog, CreateStep::Workspace);
                let offered = form.skills_offered(&catalog);
                let skills = WizardRow::Field(CreateRow::Skills);
                let name = &catalog.backends[backend].kit.name;
                let case = format!("{name}, {account:?}, {state}");
                let listed = form.wizard_rows(&catalog).contains(&skills);
                assert_eq!(listed, offered, "{case}");
                if !offered {
                    continue;
                }
                let screen = super::tests_support::render_at(&mut app, 110, 40);
                assert!(screen.contains("share skills"), "{case}: {screen}");
                focus_row(&mut app, CreateRow::Skills);
                let mode = |app: &App| match &app.screen {
                    Screen::Create(form) => form.workspace.skills,
                    _ => unreachable!("the create form is open"),
                };
                let mut seen = Vec::new();
                for code in [KeyCode::Char(' '), KeyCode::Right, KeyCode::Right] {
                    app.on_key(KeyEvent::from(code));
                    seen.push(mode(&app));
                }
                seen.sort_by_key(|m| m.flag());
                assert_eq!(
                    seen,
                    [
                        crate::sbx::SkillsMode::Off,
                        crate::sbx::SkillsMode::Readonly,
                        crate::sbx::SkillsMode::Readwrite
                    ],
                    "{case}"
                );
                let before = mode(&app);
                app.on_key(KeyEvent::from(KeyCode::Left));
                assert_eq!(mode(&app), before.prev(), "{case}");
            }
        }
    }

    #[test]
    fn the_wizard_review_shows_the_name_and_mounts_and_never_the_token() {
        let value = "wizard-review-token-value";
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        let catalog = std::sync::Arc::clone(&app.catalog);
        if let Screen::Create(form) = &mut app.screen {
            form.pick_connection(&catalog, crate::create::ConnChoice::Token);
            form.connection.account = crate::app::AccountChoice::New;
            form.connection.token_name = TextInput::new("personal");
            form.connection.token = TextInput::new(value);
            form.wizard_goto(&catalog, CreateStep::Review).unwrap();
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains(&trail(CreateStep::Review)), "{screen}");
        assert!(screen.contains("sbx name"), "{screen}");
        assert!(screen.contains("claude-token-proj"), "{screen}");
        assert!(screen.contains("Mounts"), "{screen}");
        assert!(screen.contains("/proj"), "{screen}");
        assert!(
            screen.contains("setup token · new, named personal"),
            "{screen}"
        );
        assert!(
            !screen.contains(value),
            "the token value is never drawn: {screen}"
        );

        // And on step 2, where it is typed, it stays masked.
        if let Screen::Create(form) = &mut app.screen {
            form.wizard_goto(&catalog, CreateStep::Harness).unwrap();
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains(&trail(CreateStep::Harness)), "{screen}");
        assert!(!screen.contains(value), "{screen}");
    }

    #[test]
    fn the_wizard_footer_names_its_keys() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("ctrl-s create & start"), "{screen}");
        assert!(screen.contains("esc back"), "{screen}");
        assert!(screen.contains("↵ pick & next"), "{screen}");
        assert!(screen.contains("1-7 step"), "{screen}");
        assert!(screen.contains("pgdn next"), "{screen}");
        assert!(app
            .hits
            .iter()
            .any(|(_, hit)| *hit == Hit::Press(KeyCode::Char('s'), KeyModifiers::CONTROL)));
    }

    #[test]
    fn the_wizard_footer_follows_the_row_under_the_cursor() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        let footer = |app: &mut App| {
            let screen = super::tests_support::render_at(app, 110, 40);
            screen.lines().last().unwrap_or_default().to_string()
        };
        let on_radio = footer(&mut app);
        assert!(on_radio.contains("↵ pick & next"), "{on_radio}");
        assert!(on_radio.contains("1-7 step"), "{on_radio}");
        focus_row(&mut app, CreateRow::Name);
        let on_text = footer(&mut app);
        assert!(on_text.contains("↵ next row"), "{on_text}");
        assert!(
            !on_text.contains("space"),
            "space types a space here: {on_text}"
        );
        assert!(!on_text.contains("1-7"), "a digit is typed here: {on_text}");
        focus_row(&mut app, CreateRow::Clone);
        let on_tick = footer(&mut app);
        assert!(on_tick.contains("space toggle"), "{on_tick}");
        assert!(!on_tick.contains("pick"), "{on_tick}");
        focus_row(&mut app, CreateRow::StartDir);
        let on_picker = footer(&mut app);
        assert!(on_picker.contains("↵ pick"), "{on_picker}");
        for text in [&on_radio, &on_text, &on_tick, &on_picker] {
            assert!(text.contains("ctrl-s create & start"), "{text}");
            assert!(text.contains("esc back"), "{text}");
        }
    }

    /// The frame drawn for a terminal that can show `caps`.
    fn buffer_for(
        app: &mut App,
        width: u16,
        height: u16,
        caps: theme::Caps,
    ) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::draw_in(frame, app, caps))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// One line of a frame as text.
    fn line_of(buffer: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    }

    /// The line each of `labels` is drawn on, by the text it starts with.
    fn rows_at(buffer: &ratatui::buffer::Buffer, labels: &[&str]) -> Vec<Option<u16>> {
        labels
            .iter()
            .map(|label| lines_with(buffer, label).first().map(|(_, y)| *y))
            .collect()
    }

    #[test]
    fn the_wizard_help_strip_reads_right_under_the_rows() {
        let (width, height) = (110, 40);
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Provider);
        let buffer = buffer_at(&mut app, width, height);
        let last_row = lines_with(&buffer, "LiteLLM")
            .iter()
            .map(|(_, y)| *y)
            .find(|y| *y > 2)
            .expect("the LiteLLM radio");
        let strip = strip_rule(&buffer);
        assert_eq!(strip, last_row + 2, "one blank line, then the strip");
        let ledger = lines_with(&buffer, "─ so far ")[0].1;
        let buttons = lines_with(&buffer, "[ Next ]")[0].1;
        assert!(strip < ledger, "the strip is above the ledger");
        assert_eq!(
            ledger + 8,
            buttons,
            "the ledger ends a blank line over the buttons"
        );
    }

    #[test]
    fn the_wizard_rows_stay_put_while_the_cursor_moves() {
        for (width, height) in [(110, 40), (80, 24)] {
            let mut app = app_with(Vec::new());
            open_wizard(&mut app, CreateStep::Workspace);
            let labels = [
                "label  ",
                "opens in",
                "clone mode",
                "share skills",
                "blocked hosts",
            ];
            let first = rows_at(&buffer_at(&mut app, width, height), &labels);
            assert!(first.iter().all(Option::is_some), "{width}: {first:?}");
            let rows = wizard_rows(&app);
            let mut titles = Vec::new();
            for (at, row) in rows.iter().enumerate() {
                if let Screen::Create(form) = &mut app.screen {
                    form.set_wizard_focus(at);
                }
                let buffer = buffer_at(&mut app, width, height);
                assert_eq!(rows_at(&buffer, &labels), first, "{width}: {row:?}");
                let rule = (0..height)
                    .rev()
                    .map(|y| line_of(&buffer, y))
                    .find(|l| is_strip_rule(l))
                    .expect("the strip's rule");
                titles.push(rule);
            }
            titles.dedup();
            assert_eq!(
                titles.len(),
                rows.len(),
                "{width}: the strip follows the cursor"
            );
        }
    }

    #[test]
    fn the_wizard_reads_without_colour_or_unicode() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Kits);
        if let Screen::Create(form) = &mut app.screen {
            let rtk = app.catalog.addons.iter().position(|a| a.kit.name == "rtk");
            form.kits.addons[rtk.expect("rtk is an addon")] = true;
        }
        let plain = theme::Caps {
            color: false,
            unicode: false,
        };
        // 110×40 has the ledger too.
        for (width, height) in [(80, 24), (110, 40)] {
            let buffer = buffer_for(&mut app, width, height, plain);
            let colour = buffer_at(&mut app, width, height);
            for (cell, coloured) in buffer.content.iter().zip(&colour.content) {
                assert!(cell.symbol().is_ascii(), "{:?}", cell.symbol());
                let reset = ratatui::style::Color::Reset;
                assert_eq!((cell.fg, cell.bg), (reset, reset));
                assert_eq!(
                    cell.symbol().chars().count(),
                    coloured.symbol().chars().count(),
                    "one column for one column"
                );
            }
        }
        let buffer = buffer_for(&mut app, 80, 24, plain);
        let (_, y) = lines_with(&buffer, ">[ ] host-gradle")[0];
        assert!(
            buffer[(5, y)].modifier.contains(Modifier::REVERSED),
            "the band"
        );
        assert!(!lines_with(&buffer, "[x] rtk").is_empty());
        let footer = line_of(&buffer, 23);
        assert!(footer.contains("up/down move"), "{footer}");
        assert!(footer.contains("space toggle"), "{footer}");
        let trail = line_of(&buffer, 2);
        assert!(trail.contains("*1  *2  *3  *4   5 Kits"), "{trail}");
    }

    #[test]
    fn the_wizard_trail_shortens_on_a_narrow_terminal() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Kits);
        let screen = super::tests_support::render_at(&mut app, 60, 24);
        assert!(
            screen.contains("✓1  ✓2  ✓3  ✓4   5 Kits   6  7"),
            "{screen}"
        );
        for step in CreateStep::ALL {
            let hit = Hit::WizardStep(step);
            assert!(app.hits.iter().any(|(_, h)| *h == hit), "{step:?}");
        }
        let screen = super::tests_support::render_at(&mut app, 24, 16);
        assert!(screen.contains(" 5/7 Kits "), "{screen}");
        let hit = Hit::WizardStep(CreateStep::Kits);
        assert!(app.hits.iter().any(|(_, h)| *h == hit));
    }

    #[test]
    fn a_bad_field_is_marked_once_left() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Workspace);
        focus_row(&mut app, CreateRow::Ports);
        for c in "nope".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let typing = super::tests_support::render_at(&mut app, 110, 40);
        assert!(
            !strip_text(&typing).contains("✗"),
            "not while typed: {typing}"
        );
        focus_row(&mut app, CreateRow::DenyNetwork);
        let left = super::tests_support::render_at(&mut app, 110, 40);
        assert!(left.contains("  !    ports"), "{left}");
        focus_row(&mut app, CreateRow::Ports);
        let back = super::tests_support::render_at(&mut app, 110, 40);
        assert!(
            strip_text(&back).starts_with("✗ “nope” is not a port mapping"),
            "{back}"
        );
    }

    #[test]
    fn the_review_numbers_each_step_once() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Review);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        for line in [
            " 1 Provider       ",
            " 2 Connection     ",
            " 3 Harness        ",
            " 4 Models         ",
            "   Offered        ",
            " 5 Kits           ",
            " 6 Label          ",
            "   Opens in       ",
        ] {
            assert!(screen.contains(line), "{line}: {screen}");
        }
        let buffer = buffer_at(&mut app, 110, 40);
        let (x, y) = lines_with(&buffer, " 1 Provider       ")[0];
        assert!(buffer[(x + 18, y)].modifier.contains(Modifier::BOLD));
        let (x, y) = lines_with(&buffer, " 5 Kits           ")[0];
        assert!(!buffer[(x + 18, y)].modifier.contains(Modifier::BOLD));
        app.on_key(KeyEvent::from(KeyCode::Char('5')));
        let Screen::Create(form) = &app.screen else {
            panic!("the wizard is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Kits, "5 goes there");
    }

    /// The cells of the last frame drawn at `width`×`height`, for what a test needs beyond the text:
    /// the colour a span was drawn in.
    fn buffer_at(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    /// Where `needle` starts on line `y` of `buffer`, as a column.
    fn column_of(buffer: &ratatui::buffer::Buffer, y: u16, needle: &str) -> Option<u16> {
        let line: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect();
        let at = line.find(needle)?;
        Some(line[..at].chars().count() as u16)
    }

    /// Every line of `buffer` that holds `needle`, with the column it starts at.
    /// The line of the wizard's help strip rule ([`is_strip_rule`]).
    fn strip_rule(buffer: &ratatui::buffer::Buffer) -> u16 {
        (0..buffer.area.height)
            .rev()
            .find(|y| is_strip_rule(&line_of(buffer, *y)))
            .expect("the strip's rule")
    }

    fn lines_with(buffer: &ratatui::buffer::Buffer, needle: &str) -> Vec<(u16, u16)> {
        (0..buffer.area.height)
            .filter_map(|y| column_of(buffer, y, needle).map(|x| (x, y)))
            .collect()
    }

    #[test]
    fn the_kits_step_marks_only_now_quietly_in_one_column() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Kits);
        let buffer = buffer_at(&mut app, 110, 40);
        // The kit rows' markers; the step's heading names the marker too.
        let marked: Vec<(u16, u16)> = lines_with(&buffer, crate::create::ONLY_NOW)
            .into_iter()
            .filter(|(_, y)| column_of(&buffer, *y, "[ ] ").is_some())
            .collect();
        let only_now = app
            .catalog
            .addons
            .iter()
            .filter(|a| a.create_time_only())
            .count();
        assert_eq!(
            marked.len(),
            only_now,
            "one per kit that cannot be added later"
        );
        let (column, cursor) = marked[0];
        for (x, y) in &marked {
            assert_eq!(*x, column, "line {y}: one column");
            let muted = match *y == cursor {
                true => theme::ON_SELECTION,
                false => theme::MUTED,
            };
            assert_eq!(buffer[(*x, *y)].fg, muted, "line {y}: muted");
        }
        for kit in ["jdk25", "rtk", "statusline"] {
            let (_, y) = lines_with(&buffer, &format!("] {kit} "))[0];
            assert!(
                column_of(&buffer, y, crate::create::ONLY_NOW).is_none(),
                "{kit} can be added later"
            );
        }
        // The first row has the cursor: its reason follows it.
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        let first = app
            .catalog
            .addons
            .iter()
            .find(|a| a.kit.name == "host-gradle");
        let reason = first.and_then(|a| a.create_time_reason()).unwrap();
        let note = crate::create::only_now_note(reason);
        assert!(strip_text(&screen).contains(&note), "{screen}");
    }

    #[test]
    fn the_kits_editor_marks_create_time_kits_quietly_in_one_column() {
        let mut app = app_with(populated());
        app.open_kits("claude-anthropic-proj");
        let index = |app: &App, name: &str| {
            let addons = &app.catalog.addons;
            addons.iter().position(|a| a.kit.name == name).unwrap()
        };
        let (rtk, web, jdk) = (index(&app, "rtk"), index(&app, "web"), index(&app, "jdk25"));
        let catalog = std::sync::Arc::clone(&app.catalog);
        if let Screen::Kits(form) = &mut app.screen {
            form.addons[rtk] = false;
            form.addons[web] = true;
            // The cursor on jdk25, which carries no marker: a row's place is not its kit's index.
            let rows = form.rows(&catalog);
            let jdk_row = crate::app::KitsRow::Addon(jdk);
            form.focus = rows.iter().position(|r| *r == jdk_row).unwrap();
        }
        let buffer = buffer_at(&mut app, 110, 40);
        let (_, cursor) = lines_with(&buffer, "› [x] jdk25")
            .into_iter()
            .chain(lines_with(&buffer, "›[x] jdk25"))
            .next()
            .expect("the cursor is on jdk25");
        assert!(column_of(&buffer, cursor, "create-time only").is_none());
        let marked: Vec<(u16, u16)> = lines_with(&buffer, "create-time only")
            .into_iter()
            .filter(|(_, y)| column_of(&buffer, *y, "] ").is_some())
            .collect();
        let create_time = app.catalog.addons.iter();
        let create_time = create_time.filter(|a| a.create_time_only()).count();
        assert_eq!(marked.len(), create_time, "one per create-time kit");
        for (x, y) in &marked {
            assert_eq!(*x, marked[0].0, "line {y}: one column");
            assert_eq!(buffer[(*x, *y)].fg, theme::MUTED, "line {y}: muted");
        }
        let (add, _) = lines_with(&buffer, "+ add")[0];
        let (remove, _) = lines_with(&buffer, "− remove")[0];
        assert_eq!(add, remove, "web is create-time only, rtk is not");
        assert!(add > marked[0].0, "the change comes after the marker");
    }

    #[test]
    fn the_kits_editor_names_the_agent_as_the_overview_does() {
        let mut app = app_with(populated());
        app.open_kits("claude-anthropic-proj");
        let sb = app.sandboxes[0].clone();
        for (width, beside) in [(110, true), (80, false)] {
            let buffer = buffer_at(&mut app, width, 34);
            assert!(lines_with(&buffer, "backend").is_empty(), "{width}");
            let (x, y) = lines_with(&buffer, "agent  Claude Code")[0];
            let value = x + "agent  ".chars().count() as u16;
            assert_eq!(
                buffer[(value, y)].fg,
                theme::agent_color(&sb.agent),
                "{width}"
            );
            let fixed = "fixed — create a new sandbox to change it";
            let (_, at) = lines_with(&buffer, fixed)
                .first()
                .copied()
                .unwrap_or_else(|| panic!("{width}: the whole phrase on one line"));
            assert_eq!(at == y, beside, "{width}: beside the agent, or under it");
        }
    }

    #[test]
    fn the_kits_editor_groups_the_kits_as_the_wizard_does() {
        let mut app = app_with(populated());
        app.open_kits("claude-anthropic-proj");
        let buffer = buffer_at(&mut app, 110, 40);
        let line = |text: &str| {
            let found = lines_with(&buffer, text);
            found
                .first()
                .map(|(_, y)| *y)
                .unwrap_or_else(|| panic!("{text}"))
        };
        let (hosts, others) = (line("Host caches"), line("Additional kits"));
        assert!(hosts < others, "the host caches come first");
        for addon in app.catalog.addons.iter() {
            let at = line(&format!("] {} ", addon.kit.name));
            match model::is_host_folder_kit(&addon.kit.name) {
                true => {
                    assert!(
                        hosts < at && at < others,
                        "{} is a host cache",
                        addon.kit.name
                    );
                    assert!(
                        addon.create_time_only(),
                        "{}: adding it rebuilds",
                        addon.kit.name
                    );
                }
                false => assert!(at > others, "{} after the host caches", addon.kit.name),
            }
        }
        // ↓ from the first row walks the rows down the screen.
        let mut ys = Vec::new();
        for _ in 0..app.catalog.addons.len() {
            let buffer = buffer_at(&mut app, 110, 40);
            ys.push(lines_with(&buffer, "›").first().map(|(_, y)| *y).unwrap());
            app.on_key(KeyCode::Down.into());
        }
        assert!(ys.windows(2).all(|w| w[0] < w[1]), "{ys:?}");
    }

    /// Put the wizard on `step`, with the cursor on its `at`-th row.
    fn wizard_at(app: &mut App, step: CreateStep, at: usize) {
        let catalog = std::sync::Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the wizard is open")
        };
        form.wizard_set_step(&catalog, step);
        form.set_wizard_focus(at);
    }

    /// The lines of the ledger under its rule, without the panel's first column.
    fn ledger(screen: &str) -> Vec<String> {
        let lines: Vec<String> = screen
            .lines()
            .map(|l| {
                let l = l.trim_matches('"');
                l.strip_prefix(' ').unwrap_or(l).trim_end().to_string()
            })
            .collect();
        match lines.iter().position(|l| l.starts_with(" ─ so far ")) {
            Some(rule) => lines[rule + 1..rule + 7].to_vec(),
            None => Vec::new(),
        }
    }

    #[test]
    fn the_ledger_says_what_ctrl_s_would_create() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Workspace);
        focus_row(&mut app, CreateRow::Clone);
        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        let lines = ledger(&screen);
        assert_eq!(lines.len(), 6, "{screen}");
        assert!(lines[0].starts_with("  1 Provider    LiteLLM"), "{lines:?}");
        assert!(lines[1].starts_with("  2 Connection  acme"), "{lines:?}");
        assert!(
            lines[2].starts_with("  3 Harness     Claude Code"),
            "{lines:?}"
        );
        assert!(lines[3].starts_with("  4 Models      default"), "{lines:?}");
        assert!(lines[4].starts_with("  5 Kits        none"), "{lines:?}");
        assert!(lines[5].starts_with("  6 Label       "), "{lines:?}");
        assert!(
            lines[5].ends_with("   Clone mode on"),
            "only the changed answer: {lines:?}"
        );
        let buffer = buffer_at(&mut app, 110, 40);
        let rule = lines_with(&buffer, "─ so far ")[0].1;
        for (n, step) in CreateStep::ALL[..6].iter().enumerate() {
            let y = rule + 1 + n as u16;
            let hit = app
                .hits
                .iter()
                .find(|(r, _)| r.y == y && r.x < 10)
                .map(|(_, h)| h.clone());
            assert_eq!(hit, Some(Hit::WizardStep(*step)), "line {y}");
        }
        let short = super::tests_support::render_at(&mut app, 80, 24);
        assert!(ledger(&short).is_empty(), "{short}");
        wizard_at(&mut app, CreateStep::Review, 0);
        let review = super::tests_support::render_at(&mut app, 110, 40);
        assert!(ledger(&review).is_empty(), "{review}");
    }

    /// A glyph the frame hides: drawn in its own background's colour, or on the band in anything but
    /// [`theme::ON_SELECTION`]. The caret, a reversed cell, is the band's one other colour.
    fn hidden_glyph(buffer: &ratatui::buffer::Buffer) -> Option<u16> {
        use ratatui::style::{Color, Modifier};
        let band = theme::selected().bg;
        (0..buffer.area.height).find(|&y| {
            (0..buffer.area.width).any(|x| {
                let cell = &buffer[(x, y)];
                if cell.symbol().trim().is_empty() {
                    return false;
                }
                let own = cell.fg == cell.bg && cell.fg != Color::Reset;
                let on_band = Some(cell.bg) == band
                    && !cell.modifier.contains(Modifier::REVERSED)
                    && cell.fg != theme::ON_SELECTION;
                own || on_band
            })
        })
    }

    #[test]
    fn no_wizard_text_is_drawn_in_its_own_background() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        for step in CreateStep::ALL {
            wizard_at(&mut app, step, 0);
            for at in 0..wizard_rows(&app).len() {
                wizard_at(&mut app, step, at);
                let buffer = buffer_at(&mut app, 110, 40);
                if let Some(y) = hidden_glyph(&buffer) {
                    panic!("{step:?}, row {at}: {}", line_of(&buffer, y));
                }
            }
        }
    }

    #[test]
    fn no_text_on_the_list_or_a_form_is_hidden_by_the_band() {
        let mut app = app_with(populated());
        use super::tests_support::{held, sample_secrets};
        use crate::input::KeyEvent;
        app.approvals = vec![
            held("claude-anthropic-proj", "registry.npmjs.org:443"),
            held("opencode-proj", "GET api.example.com:443/v1/data"),
        ];
        for cell in 0..app.cells.len() {
            app.cursor = cell;
            let buffer = buffer_at(&mut app, 110, 34);
            if let Some(y) = hidden_glyph(&buffer) {
                panic!("list, cell {cell}: {}", line_of(&buffer, y));
            }
        }
        app.cursor = 0;
        app.secrets = Some(sample_secrets());
        let screens: [(&str, char); 3] = [("secrets", 'S'), ("approvals", 'y'), ("kits", 'e')];
        for (name, key) in screens {
            app.screen = Screen::List;
            app.on_key(KeyEvent::from(KeyCode::Char(key)));
            assert!(!matches!(app.screen, Screen::List), "{name} opens");
            for _ in 0..40 {
                let buffer = buffer_at(&mut app, 110, 34);
                if let Some(y) = hidden_glyph(&buffer) {
                    panic!("{name}: {}", line_of(&buffer, y));
                }
                app.on_key(KeyEvent::from(KeyCode::Down));
            }
        }
    }

    #[test]
    fn the_wizard_footer_drops_whole_hints_where_they_do_not_fit() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        let rows = [
            (CreateStep::Harness, WizardRow::Agent(0)),
            (CreateStep::Kits, WizardRow::Field(CreateRow::Addon(0))),
            (CreateStep::Workspace, WizardRow::Field(CreateRow::Ports)),
            (CreateStep::Workspace, WizardRow::Next),
        ];
        for (step, row) in rows {
            wizard_at(&mut app, step, 0);
            if let Screen::Create(form) = &mut app.screen {
                assert!(form.focus_wizard_row(&app.catalog, row), "{row:?}");
            }
            for width in [60, 80, 110] {
                let screen = super::tests_support::render_at(&mut app, width, 30);
                let footer = screen.lines().last().unwrap().trim_matches('"').trim_end();
                assert!(footer.ends_with("esc back"), "{row:?} at {width}: {footer}");
            }
        }
        // At 70 columns the kit row gives up PgDn, which ↵ and the trail stand in for.
        wizard_at(&mut app, CreateStep::Kits, 0);
        let screen = super::tests_support::render_at(&mut app, 70, 30);
        let footer = screen.lines().last().unwrap();
        assert!(
            footer.contains("1-7 step") && !footer.contains("pgdn"),
            "{footer}"
        );
        let screen = super::tests_support::render_at(&mut app, 110, 30);
        assert!(screen.lines().last().unwrap().contains("pgdn next"));
    }

    #[test]
    fn the_list_footer_drops_whole_hints_and_keeps_help() {
        let mut sb = crate::model::sample("box", Some("claude-anthropic"), &[]);
        sb.running = true;
        let mut app = app_with(vec![sb]);
        for on_new in [false, true] {
            app.select(usize::from(on_new));
            assert_eq!(app.on_new_card(), on_new);
            for width in [40, 60, 80, 110, 200] {
                let screen = super::tests_support::render_at(&mut app, width, 30);
                let footer = screen.lines().last().unwrap().trim_matches('"').trim_end();
                assert!(footer.contains("? help"), "at {width}: {footer}");
                assert!(footer.starts_with(" ↵ "), "at {width}: {footer}");
                // Every hint is a key, a space and its whole label, two spaces from the next.
                let hints: Vec<&str> = footer.trim().split("  ").collect();
                let labels = [
                    "start",
                    "create",
                    "shell",
                    "new",
                    "kits",
                    "connect",
                    "start folder",
                    "account",
                    "repo skills",
                    "secrets",
                    "skills",
                    "prune",
                    "port",
                    "keep running",
                    "rename",
                    "rebuild",
                    "stop",
                    "delete",
                    "hide others",
                    "find",
                    "help",
                    "quit",
                ];
                for hint in &hints {
                    let label = hint.split_once(' ').map(|(_, l)| l).unwrap_or("");
                    assert!(
                        labels.contains(&label),
                        "at {width}, cut: {hint:?} in {footer}"
                    );
                }
            }
        }
        // Wide enough, nothing gives way.
        app.select(0);
        let screen = super::tests_support::render_at(&mut app, 240, 30);
        assert!(screen.lines().last().unwrap().contains("q quit"));
        assert!(screen.lines().last().unwrap().contains("/ find"));
    }

    fn type_keys(app: &mut App, text: &str) {
        for c in text.chars() {
            app.on_key(crate::input::KeyEvent::from(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_search_owns_the_footer_line() {
        let mut app = app_with(vec![
            crate::model::sample("api-one", Some("claude-anthropic"), &[]),
            crate::model::sample("api-two", Some("claude-anthropic"), &[]),
            crate::model::sample("docs", Some("claude-anthropic"), &[]),
        ]);
        app.set_info("Stopped docs");
        type_keys(&mut app, "/");
        let buffer = buffer_at(&mut app, 110, 24);
        let footer = line_of(&buffer, 23);
        assert!(footer.starts_with(" / "), "{footer}");
        assert!(footer.contains(super::FIND_PROMPT), "{footer}");
        assert!(!footer.contains("Stopped"), "{footer}");

        type_keys(&mut app, "api");
        let buffer = buffer_at(&mut app, 110, 24);
        let footer = line_of(&buffer, 23);
        assert!(footer.starts_with(" /api "), "{footer}");
        assert!(footer.contains("1 of 2"), "{footer}");
        for hint in ["↑↓ matches", "↵ keep", "esc back"] {
            assert!(footer.contains(hint), "{hint}: {footer}");
        }
        let caret = column_of(&buffer, 23, "/api").unwrap() + 4;
        assert!(buffer[(caret, 23)].modifier.contains(Modifier::REVERSED));

        type_keys(&mut app, "z");
        let buffer = buffer_at(&mut app, 110, 24);
        let footer = line_of(&buffer, 23);
        let at = column_of(&buffer, 23, "no match").expect(&footer);
        assert_eq!(buffer[(at, 23)].fg, theme::WARN, "{footer}");

        // The search over, the status it covered is back until it times out.
        app.on_key(crate::input::KeyEvent::from(KeyCode::Esc));
        let screen = super::tests_support::render_at(&mut app, 110, 24);
        assert!(screen.lines().last().unwrap().contains("Stopped docs"));
    }

    #[test]
    fn a_search_marks_what_it_found_in_the_list() {
        let mut far = crate::model::sample("far", Some("claude-anthropic"), &[]);
        far.workspace = "/work/payments".into();
        let mut app = app_with(vec![
            crate::model::sample("Payments", Some("claude-anthropic"), &[]),
            far,
        ]);
        type_keys(&mut app, "/pay");
        let buffer = buffer_at(&mut app, 110, 30);
        let reversed = |y: u16| -> String {
            (0..buffer.area.width)
                .filter(|&x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect()
        };
        let label = lines_with(&buffer, "Payments")[0].1;
        assert_eq!(reversed(label), "Pay", "{}", line_of(&buffer, label));
        let heading = lines_with(&buffer, "/work/payments")[0].1;
        assert_eq!(reversed(heading), "pay", "{}", line_of(&buffer, heading));

        app.on_key(crate::input::KeyEvent::from(KeyCode::Esc));
        let buffer = buffer_at(&mut app, 110, 30);
        let label = lines_with(&buffer, "Payments")[0].1;
        assert!(
            !(0..buffer.area.width)
                .any(|x| buffer[(x, label)].modifier.contains(Modifier::REVERSED)),
            "no marks once the search ends"
        );
    }

    #[test]
    fn a_wizard_key_repaints_only_the_lines_it_changes() {
        let (width, height) = (110, 40);
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Kits);
        let changed = |a: &ratatui::buffer::Buffer, b: &ratatui::buffer::Buffer| -> Vec<u16> {
            (0..height)
                .filter(|y| (0..width).any(|x| a[(x, *y)] != b[(x, *y)]))
                .collect()
        };
        let before = buffer_at(&mut app, width, height);
        let strip = strip_rule(&before);
        let buttons = lines_with(&before, "[ Next ]")[0].1;
        let rule = lines_with(&before, "─ so far ")[0].1;
        let row = |b: &ratatui::buffer::Buffer, kit: &str| lines_with(b, &format!("] {kit} "))[0].1;
        let (gradle, maven) = (row(&before, "host-gradle"), row(&before, "host-maven"));

        app.on_key(KeyEvent::from(KeyCode::Down));
        let after = buffer_at(&mut app, width, height);
        let moved = changed(&before, &after);
        assert!(
            moved.contains(&gradle) && moved.contains(&maven),
            "{moved:?}"
        );
        for y in &moved {
            let ok = [gradle, maven].contains(y) || (strip..buttons).contains(y);
            assert!(ok, "line {y} changed: {}", line_of(&after, *y));
        }

        app.on_key(KeyEvent::from(KeyCode::Char(' ')));
        let ticked = buffer_at(&mut app, width, height);
        let ticked_lines = changed(&after, &ticked);
        assert!(ticked_lines.contains(&maven), "{ticked_lines:?}");
        for y in &ticked_lines {
            let ok = *y == maven || ((strip..buttons).contains(y) && *y != rule);
            assert!(ok, "line {y} changed: {}", line_of(&ticked, *y));
        }
    }

    #[test]
    fn a_form_key_repaints_only_the_two_rows_and_the_strip() {
        let (width, height) = (110, 50);
        // How each form is opened, from the list.
        type Open = fn(&mut App);
        let forms: [(&str, Open); 5] = [
            ("secrets", |app: &mut App| {
                app.secrets = Some(super::tests_support::sample_secrets());
                app.on_key(KeyEvent::from(KeyCode::Char('S')));
            }),
            ("skills", |app: &mut App| {
                app.skills = Some(crate::sbx::SkillsStore {
                    store: "/store".into(),
                    skills: vec!["frontend-design".into(), "pdf".into()],
                });
                app.on_key(KeyEvent::from(KeyCode::Char('K')));
            }),
            ("approvals", |app: &mut App| {
                app.approvals = vec![
                    super::tests_support::held("claude-anthropic-proj", "example.org:443"),
                    super::tests_support::held("opencode-proj", "example.net:80"),
                ];
                app.on_key(KeyEvent::from(KeyCode::Char('y')));
            }),
            ("connect", |app: &mut App| {
                app.screen = Screen::Connect(crate::app::ConnectForm {
                    sandbox: "claude-anthropic-proj".into(),
                    ssh_ready: true,
                    focus: 0,
                });
            }),
            ("kits", |app: &mut App| {
                app.open_kits("claude-anthropic-proj")
            }),
        ];
        let changed = |a: &ratatui::buffer::Buffer, b: &ratatui::buffer::Buffer| -> Vec<u16> {
            (0..height)
                .filter(|y| (0..width).any(|x| a[(x, *y)] != b[(x, *y)]))
                .collect()
        };
        // The cursor's row: the line whose first column inside the panel holds its mark.
        let cursor = |b: &ratatui::buffer::Buffer| -> u16 {
            (0..height)
                .find(|y| b[(2, *y)].symbol() == form::CURSOR)
                .expect("the cursor's row is on screen")
        };
        for (name, open) in forms {
            let mut app = app_with(populated());
            open(&mut app);
            let before = buffer_at(&mut app, width, height);
            let rule = lines_with(&before, "  ─ ").last().map(|(_, y)| *y);
            let rule = rule.unwrap_or_else(|| panic!("{name}: a strip"));
            // The panel's last inner line, over its border and the footer.
            let buttons = height - 3;
            let from = cursor(&before);
            app.on_key(KeyEvent::from(KeyCode::Down));
            let after = buffer_at(&mut app, width, height);
            let to = cursor(&after);
            assert_ne!(from, to, "{name}: the cursor moved");
            for y in changed(&before, &after) {
                let ok = [from, to, height - 1].contains(&y) || (rule..buttons).contains(&y);
                assert!(ok, "{name}: line {y} changed: {}", line_of(&after, y));
            }
        }
    }

    #[test]
    fn every_wizard_step_draws_at_any_size() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Harness);
        for step in [
            CreateStep::Harness,
            CreateStep::Harness,
            CreateStep::Kits,
            CreateStep::Workspace,
        ] {
            let last = wizard_rows(&app).len();
            for at in [0, last / 2] {
                wizard_at(&mut app, step, at);
                for width in [8, 20, 40, 60, 80, 110, 200] {
                    for height in [4, 8, 12, 16, 24, 40, 60] {
                        let screen = super::tests_support::render_at(&mut app, width, height);
                        if width < 20 || height < 16 {
                            continue;
                        }
                        let marked = screen
                            .lines()
                            .any(|l| l.trim_matches('"').starts_with("  ›"));
                        let on_buttons = app.on_form_buttons();
                        assert!(marked || on_buttons, "{step:?} {width}×{height}: {screen}");
                    }
                }
            }
        }
    }

    #[test]
    fn an_interrupted_wizard_comes_back_on_screen_as_it_was() {
        let (width, height) = (110, 40);
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Workspace);
        focus_row(&mut app, CreateRow::Ports);
        for c in "8080:3".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let before = buffer_at(&mut app, width, height);
        buffer_at(&mut app, 60, 20);
        assert_eq!(buffer_at(&mut app, width, height), before, "after a resize");

        app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(matches!(app.screen, Screen::List), "Ctrl-C closes it");
        buffer_at(&mut app, 70, 20);
        app.on_key(KeyEvent::from(KeyCode::Char('n')));
        let after = buffer_at(&mut app, width, height);
        for y in 0..height - 1 {
            assert_eq!(line_of(&after, y), line_of(&before, y), "line {y}");
        }
        let footer = line_of(&after, height - 1);
        assert!(footer.contains(crate::create::DRAFT_BACK), "{footer}");
    }

    #[test]
    fn the_generated_label_is_tagged_until_typed() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Workspace);
        focus_row(&mut app, CreateRow::Name);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(
            screen.contains("label         Claude Code LiteLLM   generated"),
            "{screen}"
        );
        for c in "api".chars() {
            app.on_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("label         api "), "{screen}");
        assert!(!screen.contains("generated"), "{screen}");
    }

    #[test]
    fn the_review_mutes_defaults_and_leads_back_to_their_step() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Review);
        let buffer = buffer_at(&mut app, 110, 40);
        let value = |label: &str| {
            let (x, y) = lines_with(&buffer, &format!(" {label:<15}"))[0];
            buffer[(x + 16, y)].fg
        };
        assert_eq!(value("Kits"), theme::MUTED, "nothing ticked");
        assert_eq!(value("Ports"), theme::MUTED, "nothing typed");
        assert_ne!(value("Harness"), theme::MUTED, "what the sandbox is");
        assert_ne!(value("Label"), theme::MUTED, "what the sandbox is called");

        let (_, y) = lines_with(&buffer, " Kits           ")[0];
        click_at(&mut app, (10, y));
        let Screen::Create(form) = &app.screen else {
            panic!("the wizard is open")
        };
        assert_eq!(form.wizard_step(), CreateStep::Kits);
    }

    #[test]
    fn the_wizard_draws_its_main_action_as_the_primary_button() {
        let mut app = app_with(Vec::new());
        open_wizard(&mut app, CreateStep::Provider);
        let buffer = buffer_at(&mut app, 110, 40);
        let (x, y) = lines_with(&buffer, "[ Next ]")[0];
        assert_eq!(buffer[(x, y)].fg, theme::ACCENT);
        let (back, y) = lines_with(&buffer, "  Back  ")[0];
        assert!(back > x, "Next comes first");
        assert_eq!(buffer[(back + 2, y)].fg, theme::MUTED, "inert on step 1");
        open_wizard(&mut app, CreateStep::Kits);
        let buffer = buffer_at(&mut app, 110, 40);
        let (back, y) = lines_with(&buffer, "  Back  ")[0];
        assert_eq!(buffer[(back + 2, y)].fg, theme::ACCENT);
        open_wizard(&mut app, CreateStep::Review);
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("[ Create & start ]"), "{screen}");
        assert!(screen.contains("  Create only  "), "{screen}");
    }

    #[test]
    fn the_repo_skills_screens_render() {
        use crate::repo_skills_ui::{RepoScope, RepoSkillsForm, RepoView};
        use aisbx_core::skill_select::{Pick, Selection, Skills};
        let (r, s) = ("github-com-o-r-0123abcd", "github-com-o-s-89abcdef");
        let repos = vec![
            RepoView {
                id: r.into(),
                source: "o/r".into(),
                commit: Some("0123456789abcdef".into()),
                problem: None,
                skills: vec!["alpha".into(), "beta".into()],
                skipped: vec![("Bad_x".into(), "bad-name")],
            },
            RepoView {
                id: s.into(),
                source: "o/s".into(),
                commit: None,
                problem: Some(format!(
                    "{s} has no cache git can read; run `aisbx skill-repo fetch {s}`"
                )),
                skills: Vec::new(),
                skipped: Vec::new(),
            },
        ];
        let named = |name: &str| Pick {
            repo: r.into(),
            skills: Skills::Named(vec![name.into()]),
        };
        let mut sel = Selection {
            global: vec![named("alpha")],
            ..Selection::default()
        };
        sel.sandboxes.insert("box".into(), vec![named("beta")]);
        let line_of = |screen: &str, text: &str| -> String {
            screen
                .lines()
                .find(|l| l.contains(text))
                .unwrap_or_else(|| panic!("{text} is on screen:\n{screen}"))
                .to_string()
        };

        let mut app = app_with(Vec::new());
        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            RepoScope::Sandbox("box".into()),
            repos.clone(),
            Ok(sel.clone()),
        ));
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("Repo skills · box"), "{screen}");
        assert!(line_of(&screen, "[x] alpha").contains("global"), "{screen}");
        assert!(!line_of(&screen, "[x] beta").contains("global"), "{screen}");
        assert!(line_of(&screen, s).contains("not fetched"), "{screen}");
        assert!(screen.contains("aisbx skill-repo fetch"), "{screen}");
        assert!(screen.contains("skipped: bad-name"), "{screen}");
        for text in ["Sync now", "Save", "Close", "space tick"] {
            assert!(screen.contains(text), "{text}:\n{screen}");
        }

        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            RepoScope::Global,
            repos.clone(),
            Ok(sel),
        ));
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("Repo skills · all sandboxes"), "{screen}");
        assert!(screen.contains("existing and new"), "{screen}");
        assert!(
            !line_of(&screen, "[x] alpha").contains("global"),
            "{screen}"
        );
        assert!(line_of(&screen, "[ ] beta").contains("beta"), "{screen}");
        assert!(!screen.contains("Sync now"), "{screen}");

        app.screen = Screen::RepoSkills(RepoSkillsForm::from_parts(
            RepoScope::Global,
            repos,
            Err("skill-selection.json does not parse; fix or remove it".into()),
        ));
        let screen = super::tests_support::render_at(&mut app, 110, 40);
        assert!(screen.contains("cannot be read"), "{screen}");
        assert!(screen.contains("does not parse"), "{screen}");
        assert!(!screen.contains("all skills"), "{screen}");
        assert!(!screen.contains("Save"), "{screen}");
    }
}

/// The Connect screen pinned line by line: its rules live in core (`App::connect_view`).
#[cfg(test)]
mod connect_render {
    use super::*;
    use crate::app::ConnectForm;
    use crate::kits::Catalog;
    use crate::model::Sandbox;

    /// `ssh`, `code` and `cursor` found first on PATH, whatever this host has installed, so the
    /// rows read *↵ launches it* everywhere. Only ever prepended: nothing else is shadowed.
    fn launchers_on_path() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let dir =
                std::env::temp_dir().join(format!("aisbx-tui-launchers-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            for name in ["ssh", "code", "cursor", "ssh.exe", "code.cmd", "cursor.cmd"] {
                std::fs::write(dir.join(name), "").unwrap();
            }
            let path = std::env::var_os("PATH").unwrap_or_default();
            let paths = std::iter::once(dir).chain(std::env::split_paths(&path));
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        });
    }

    /// The Connect panel's text: its title, then each line inside the border with trailing
    /// blanks trimmed, up to the last that holds anything.
    fn connect_screen(sb: Sandbox, ssh_ready: bool, focus: usize) -> String {
        launchers_on_path();
        let name = sb.name.clone();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.screen = Screen::Connect(ConnectForm {
            sandbox: name,
            ssh_ready,
            focus,
        });
        let screen = super::tests_support::render_at(&mut app, 110, 30);
        let lines: Vec<String> = screen
            .lines()
            .map(|l| l.trim_matches('"').to_string())
            .skip_while(|l| !l.contains("Connect to"))
            .collect();
        let mut out = vec![lines[0].trim_matches(|c| "━ ".contains(c)).to_string()];
        for line in &lines[1..] {
            let chars: Vec<char> = line.chars().collect();
            if chars.first() == Some(&'─') {
                break;
            }
            let inside: String = chars[1..chars.len() - 1].iter().collect();
            out.push(inside.trim_end().to_string());
        }
        while out.last().is_some_and(|l| l.is_empty()) {
            out.pop();
        }
        out.join("\n")
    }

    const SSH_MISSING: &str = "\
Connect to box
 SSH endpoint
    box.sbx   stopped — connecting starts it
      Workspace inside the sandbox: /proj
      — its host path, not ~/workspace. Point the app's folder picker there.

      ⚠ `sbx setup ssh` has not written its block to ~/.ssh/config yet, so
        nothing below can connect.
 › Set up SSH now

 Connect with
    Terminal (ssh)   ↵ launches it
    VS Code   ↵ launches it
    Cursor   ↵ launches it
    IntelliJ IDEA / JetBrains   set up inside the app — steps below
    Claude Desktop   set up inside the app — steps below
 ─ Set up SSH now ─────────────────────────────────────────────────────────────────────────────────────────
 Adds one managed `Host *.sbx` block. Idempotent, and needs no SSH key —
 the connection rides the daemon's socket and your Docker login.







   Close";

    const VS_CODE_FOCUSED: &str = "\
Connect to box
 SSH endpoint
    box.sbx   running
      Workspace inside the sandbox: /proj
      — its host path, not ~/workspace. Point the app's folder picker there.

 Connect with
    Terminal (ssh)   ↵ launches it
 ›  VS Code   ↵ launches it
    Cursor   ↵ launches it
    IntelliJ IDEA / JetBrains   set up inside the app — steps below
    Claude Desktop   set up inside the app — steps below
 ─ VS Code ────────────────────────────────────────────────────────────────────────────────────────────────
 Needs the Remote - SSH extension (ms-vscode-remote.remote-ssh).
 Runs:  code --remote ssh-remote+box.sbx /proj
 The first connect downloads the VS Code server inside the sandbox.
 ⚠ this sandbox has no remote-dev kit, so the server download is blocked by the network policy. Add it with
 e, then try again.








   Close";

    const MOUNTLESS: &str = "\
Connect to bare
 SSH endpoint
    bare.sbx   stopped — connecting starts it
      Workspace inside the sandbox: /home/agent/workspace
      — the image's own working directory: nothing from the host is mounted.

 Connect with
 ›  Terminal (ssh)   ↵ launches it
    VS Code   ↵ launches it
    Cursor   ↵ launches it
    IntelliJ IDEA / JetBrains   set up inside the app — steps below
    Claude Desktop   set up inside the app — steps below
 ─ Terminal (ssh) ─────────────────────────────────────────────────────────────────────────────────────────
 Runs `ssh bare.sbx` in a terminal, starting in the workspace.
 The workspace is at /home/agent/workspace.
 Also usable as `sftp bare.sbx` and `scp file bare.sbx:/home/agent/workspace`.










   Close";

    /// SSH not set up, on a stopped sandbox; SSH ready with VS Code focused on a running sandbox
    /// without `remote-dev`; a mountless sandbox with the terminal focused.
    #[test]
    fn the_connect_screen_is_pinned() {
        let sb = model::sample("box", Some("claude-anthropic"), &[]);
        assert_eq!(connect_screen(sb.clone(), false, 0), SSH_MISSING);
        let running = Sandbox {
            running: true,
            ..sb
        };
        assert_eq!(connect_screen(running, true, 1), VS_CODE_FOCUSED);
        let mut bare = model::sample("bare", Some("claude-anthropic"), &[]);
        bare.workspace = std::path::PathBuf::new();
        assert_eq!(connect_screen(bare, true, 0), MOUNTLESS);
    }
}

/// `cargo test -- --ignored --nocapture preview` prints each screen as plain text — a quick way
/// to eyeball the layout without launching a terminal.
#[cfg(test)]
mod preview {
    use super::tests_support::*;

    #[test]
    #[ignore = "prints the screens; run explicitly"]
    fn preview() {
        for (title, screen) in screens() {
            println!("\n=== {title} ===\n{screen}");
        }
    }
}

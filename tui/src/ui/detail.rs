//! The tabbed details pane for the selected sandbox.
//! Overview and Mounts draw from the list snapshot; Resources, Network, Filesystem and Activity
//! draw what [`crate::insight`] has fetched.

use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use super::{bordered, clip, panel, Hits};
use crate::app::{App, Hit};
use crate::insight::{self, short_time, Kind, Note, PieceKind, SourceTone, Tab};
use crate::model::{self, Auth, FolderRelation, Lifecycle, Sandbox, Skills};
use crate::sbx::{Approval, LogHost, PolicyRule, SkillsMode};
use crate::state;
use crate::stats;
use crate::theme;

pub(super) fn render(frame: &mut Frame, app: &mut App, area: Rect) -> Hits {
    let mut hits = Hits::new();
    let Some(sb) = app.selected() else {
        // The cursor is on the New row.
        let block = panel("New sandbox", false);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let lines = Paragraph::new(new_sandbox_lines(app)).wrap(Wrap { trim: true });
        frame.render_widget(lines, inner);
        return hits;
    };

    let label = sb.label.clone();
    let block = bordered(app.detail_full);
    let body_area = block.inner(area);
    if body_area.height == 0 || body_area.width == 0 {
        frame.render_widget(block, area);
        return hits;
    }
    hits.push((body_area, Hit::ScrollDetail));

    let width = body_area.width as usize;
    let lines = match app.tab {
        Tab::Overview => overview_in(sb, app, width),
        Tab::Resources => resources(sb, app, width),
        Tab::Mounts => mounts(sb),
        Tab::Ports => ports(sb, app),
        Tab::Network => network(sb, app, width),
        Tab::Filesystem => filesystem(sb, app, width),
        Tab::Activity => activity(app),
    };

    // Wrapped here, not by the paragraph: the scroll range must count the lines as drawn.
    let lines = hang(lines, width);
    // Clamp here rather than on key-press: only rendering knows the viewport height.
    let max_scroll = (lines.len() as u16).saturating_sub(body_area.height);
    app.detail_scroll = app.detail_scroll.min(max_scroll);
    let hidden = Hidden {
        above: app.detail_scroll as usize,
        below: lines
            .len()
            .saturating_sub(app.detail_scroll as usize + body_area.height as usize),
    };
    let block = match scroll_hint(hidden, app.detail_full) {
        Some(hint) => block.title_bottom(hint.right_aligned()),
        None => block,
    };
    frame.render_widget(block, area);
    hits.extend(render_tabs(frame, app.tab, &label, area));
    frame.render_widget(
        Paragraph::new(lines).scroll((app.detail_scroll, 0)),
        body_area,
    );
    hits
}

/// How many of a tab's lines are scrolled off above and below the pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Hidden {
    above: usize,
    below: usize,
}

/// What the pane's bottom border says while lines are off screen: how many, and the key that
/// brings them in — the arrows once the pane is maximized, Ctrl and an arrow beside the list.
fn scroll_hint(hidden: Hidden, full: bool) -> Option<Line<'static>> {
    if hidden.above == 0 && hidden.below == 0 {
        return None;
    }
    let mut spans = Vec::new();
    if hidden.above > 0 {
        spans.push(Span::styled(format!(" ↑ {}", hidden.above), theme::muted()));
    }
    if hidden.below > 0 {
        spans.push(Span::styled(
            format!(" ↓ {} more", hidden.below),
            theme::muted(),
        ));
    }
    let key = match full {
        true => "↑↓",
        false => "ctrl-↑↓",
    };
    spans.push(Span::styled("  ", theme::muted()));
    spans.push(Span::styled(key, theme::key()));
    spans.push(Span::styled(" scroll ", theme::muted()));
    Some(Line::from(spans))
}

/// Where a wrapped line goes on: under the value of a `field` row (or a row whose first piece is
/// a label set off by two spaces), else under its own indentation, at most half the width.
fn hang_indent(line: &Line<'_>, width: usize) -> usize {
    let first = line.spans.first().map(|s| s.content.as_ref()).unwrap_or("");
    let indent = match line.spans.len() > 1 && first.ends_with("  ") && !first.trim().is_empty() {
        true => first.chars().count(),
        false => {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            text.chars().take_while(|c| *c == ' ').count()
        }
    };
    indent.min(width / 2)
}

/// `lines` wrapped to `width` columns at spaces, each continuation indented by
/// [`hang_indent`], styles kept; a word longer than a line is cut where the line ends.
fn hang(lines: Vec<Line<'static>>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        if line.width() <= width {
            out.push(line);
            continue;
        }
        let indent = hang_indent(&line, width);
        let style = line.style;
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0;
        let mut flush = |spans: &mut Vec<Span<'static>>, used: &mut usize| {
            // The space a line was broken at is not drawn.
            if let Some(last) = spans.last_mut() {
                let kept = last.content.trim_end().len();
                last.content.to_mut().truncate(kept);
            }
            let mut done = Line::from(std::mem::take(spans));
            done.style = style;
            out.push(done);
            spans.push(Span::raw(" ".repeat(indent)));
            *used = indent;
        };
        for span in line.spans {
            for word in span.content.split_inclusive(' ') {
                let mut word = word.to_string();
                loop {
                    let fits = word.trim_end().chars().count();
                    // A continuation never starts with the space that ended the line before.
                    if used == indent && !spans.is_empty() && word.trim().is_empty() {
                        break;
                    }
                    if used + fits <= width {
                        used += word.chars().count();
                        spans.push(Span::styled(word, span.style));
                        break;
                    }
                    if used > indent {
                        flush(&mut spans, &mut used);
                        continue;
                    }
                    // Longer than a whole line: cut it where the line ends.
                    let room = width - used;
                    let head: String = word.chars().take(room).collect();
                    word = word.chars().skip(room).collect();
                    spans.push(Span::styled(head, span.style));
                    flush(&mut spans, &mut used);
                    if word.is_empty() {
                        break;
                    }
                }
            }
        }
        let mut last = Line::from(spans);
        last.style = style;
        out.push(last);
    }
    out
}

/// What the pane says on the New row: the key, the folder, and the wizard's five steps as its
/// trail names them.
fn new_sandbox_lines(app: &App) -> Vec<Line<'static>> {
    let mut steps = vec![Span::raw("   ")];
    for step in crate::create::CreateStep::ALL {
        steps.push(Span::styled(step.number().to_string(), theme::key()));
        steps.push(Span::raw(format!(" {}   ", step.title())));
    }
    vec![
        Line::from(vec![
            Span::styled("↵", theme::key()),
            Span::styled("  create a sandbox in  ", theme::muted()),
            Span::raw(model::tilde(&app.cwd)),
        ]),
        Line::from(""),
        Line::from(steps),
        Line::from(""),
        note("Nothing is created before the Review; the sandbox is named after this folder."),
    ]
}

/// The tab strip's width with every title.
fn strip_width() -> usize {
    Tab::ALL
        .iter()
        .map(|tab| tab.title().chars().count() + 4)
        .sum()
}

/// The least room the label is drawn in, its two spaces included: less names nothing.
const LABEL_ROOM: usize = 8;

/// The pane's top border: the seven tabs (only the open one titled where all titles do not fit),
/// and the sandbox's label at its right end, cut to the room the tabs leave.
fn render_tabs(frame: &mut Frame, active: Tab, label: &str, pane: Rect) -> Hits {
    let mut hits = Hits::new();
    // Between the corners, a dash of the border after the left one.
    let area = Rect {
        x: pane.x + 2,
        y: pane.y,
        width: pane.width.saturating_sub(3),
        height: 1,
    };
    let short = strip_width() + 1 > area.width as usize;
    let mut spans = Vec::new();
    let mut x = area.x;
    for (i, tab) in Tab::ALL.iter().enumerate() {
        let text = match short && *tab != active {
            true => format!(" {} ", i + 1),
            false => format!(" {} {} ", i + 1, tab.title()),
        };
        let width = text.chars().count() as u16;
        spans.push(Span::styled(
            text,
            if *tab == active {
                Style::default()
                    .fg(theme::ACCENT)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD)
            } else {
                theme::muted()
            },
        ));
        if x < area.right() {
            hits.push((
                Rect {
                    x,
                    y: area.y,
                    width: width.min(area.right() - x),
                    height: 1,
                },
                Hit::TabAt(i),
            ));
        }
        x = x.saturating_add(width);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    // A border's dash on each side of the label, as a panel's title has.
    let room = (area.right().saturating_sub(x) as usize).saturating_sub(2);
    if room >= LABEL_ROOM {
        // Cut where a word ends, if the cut falls after one: `Host…`, not `Host …`.
        let cut = clip(label, room - 2);
        let cut = match cut.strip_suffix('…') {
            Some(head) => format!("{}…", head.trim_end()),
            None => cut,
        };
        let text = format!(" {cut} ");
        let width = text.chars().count() as u16;
        let at = Rect {
            x: area.right().saturating_sub(width + 1),
            width,
            ..area
        };
        frame.render_widget(Paragraph::new(Span::styled(text, theme::title())), at);
    }
    hits
}

// --- tabs ---------------------------------------------------------------------------------

/// The Overview's ledger, read from the top: what needs you first, then what the sandbox is,
/// where it works, its names, its history, and how it was built. On a pane `width` wide it is
/// one column, or two where both fit: the record (names, history, build) right of the rest.
fn overview_in(sb: &Sandbox, app: &App, width: usize) -> Vec<Line<'static>> {
    let (left, right) = overview_parts(sb, app);
    side_by_side(left, right, width)
}

/// What stands between the Overview's two columns.
const COLUMN_RULE: &str = "  │";

/// The fewest cells the Overview's first column keeps beside the second.
const LEFT_MIN: usize = 56;

/// `left` and `right` as two columns, `right` after a rule, where `right` fits beside at least
/// [`LEFT_MIN`] cells of `left`, whose lines then wrap under their values; else `right` under
/// `left`.
fn side_by_side(
    left: Vec<Line<'static>>,
    right: Vec<Line<'static>>,
    width: usize,
) -> Vec<Line<'static>> {
    let wide = |lines: &[Line]| lines.iter().map(|l| l.width()).max().unwrap_or(0);
    let wr = wide(&right);
    let room = width.saturating_sub(wr + COLUMN_RULE.chars().count());
    if right.is_empty() || room < LEFT_MIN {
        let mut left = left;
        left.extend(right);
        return left;
    }
    let mut left = hang(left, room);
    let wl = wide(&left);
    let rows = left.len().max(right.len());
    left.resize(rows, Line::from(""));
    let mut right = right.into_iter();
    (left.into_iter())
        .map(|line| {
            let pad = wl - line.width();
            let mut spans = line.spans;
            spans.push(Span::raw(" ".repeat(pad)));
            spans.push(Span::styled(COLUMN_RULE, theme::muted()));
            spans.extend(right.next().map(|l| l.spans).unwrap_or_default());
            Line::from(spans)
        })
        .collect()
}

/// The Overview's two parts: what needs you and what the sandbox is, then its record.
fn overview_parts(sb: &Sandbox, app: &App) -> (Vec<Line<'static>>, Vec<Line<'static>>) {
    let relation = sb.folder_relation(&app.cwd);
    let (here, mountless) = (
        relation == FolderRelation::Here,
        relation == FolderRelation::Mountless,
    );
    let mut lines = Vec::new();

    lines.push(field("status", status_spans(sb)));

    let waiting = app.approvals_of(&sb.name);
    if !waiting.is_empty() {
        lines.push(field("⚑ network", waiting_spans(&waiting)));
    }

    for service in app.missing_credentials(sb) {
        lines.push(field(
            "⚠ secret",
            vec![Span::styled(
                format!("{service} is not stored — run  sbx secret set {service}"),
                theme::danger(),
            )],
        ));
    }

    let lifecycle = sb.lifecycle();
    if lifecycle == Some(Lifecycle::KeepsRunning) {
        lines.push(field(
            "lifecycle",
            vec![
                Span::raw("keeps running after its sessions end"),
                Span::styled(
                    "   detached mode, until s stops it — b rebuilds it back",
                    theme::muted(),
                ),
            ],
        ));
    } else if lifecycle == Some(Lifecycle::StopsWithPorts) {
        lines.push(field(
            "lifecycle",
            vec![
                Span::raw("stops itself after its last session"),
                Span::styled(
                    "   and its ports stop answering — b keeps it running",
                    theme::muted(),
                ),
            ],
        ));
    }

    let agent = match app.backend_of(sb) {
        Some(b) => vec![
            Span::styled(
                b.kit.display_name.clone(),
                Style::default().fg(theme::agent_color(&b.agent)),
            ),
            Span::styled(format!("  — {}", b.setup_hint()), theme::muted()),
        ],
        None => vec![
            Span::styled(
                format!("{} (not from this repo's kits)", sb.agent),
                theme::warn(),
            ),
            Span::styled(
                if sb.kits_unknown {
                    "  — created outside aisbx"
                } else {
                    ""
                },
                theme::muted(),
            ),
        ],
    };
    lines.push(field("agent", agent));

    if let Some(conn) = &sb.litellm {
        let mut spans = vec![Span::raw(conn.clone())];
        if app.litellm_key_missing(conn) {
            spans.push(Span::styled(
                "  ⚠ key missing — aisbx litellm add <file> --key-stdin --replace",
                theme::danger(),
            ));
        }
        lines.push(field("connection", spans));
    }

    if let Some(auth) = &sb.auth {
        let users = auth.token_name().map(|t| app.token_users(t)).unwrap_or(0);
        lines.push(field("account", account_spans(auth, users)));
    }

    let pinned = app.pinned_models(sb);
    if !pinned.is_empty() {
        let mut spans = Vec::new();
        for (alias, name) in pinned {
            if !spans.is_empty() {
                spans.push(Span::raw("   "));
            }
            spans.push(Span::styled(
                format!("{alias} "),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::styled(name, theme::muted()));
        }
        lines.push(field("models", spans));
    }

    let mut kit_spans: Vec<Span> = Vec::new();
    for name in &sb.addons {
        if !kit_spans.is_empty() {
            kit_spans.push(Span::raw("   "));
        }
        let display = app
            .catalog
            .addon(name)
            .map(|a| a.kit.display_name.clone())
            .unwrap_or_else(|| name.clone());
        kit_spans.push(Span::styled(
            name.to_string(),
            Style::default()
                .fg(theme::kit_color(name))
                .add_modifier(Modifier::BOLD),
        ));
        kit_spans.push(Span::styled(format!(" {display}"), theme::muted()));
    }
    for name in &sb.foreign_kits {
        if !kit_spans.is_empty() {
            kit_spans.push(Span::raw("   "));
        }
        kit_spans.push(Span::styled(name.clone(), theme::warn()));
    }
    if kit_spans.is_empty() {
        kit_spans.push(Span::styled("none", theme::muted()));
    }
    lines.push(field("kits", kit_spans));
    // Directly under the kits they are about.
    lines.extend(warnings(sb));

    lines.push(field(
        "folder",
        vec![
            Span::raw(sb.folder_label()),
            Span::styled(
                if mountless {
                    format!(
                        "   the agent works in {} inside the sandbox",
                        model::MOUNTLESS_WORKDIR
                    )
                } else if here {
                    "   this folder".to_string()
                } else {
                    "   another folder".to_string()
                },
                if here || mountless {
                    theme::muted()
                } else {
                    theme::warn()
                },
            ),
        ],
    ));

    if sb.start_dir.is_some() {
        let gone = sb.start_dir_missing();
        lines.push(field(
            "opens in",
            vec![
                Span::styled(
                    sb.start_dir
                        .as_deref()
                        .map(|r| r.display().to_string())
                        .unwrap_or_default(),
                    if gone {
                        theme::warn()
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    match gone {
                        true => "   ⚠ gone — starts at the folder above until w picks another",
                        false => "   the agent's working directory, inside the folder above",
                    },
                    if gone { theme::warn() } else { theme::muted() },
                ),
            ],
        ));
    }

    if let Some(skills) = sb.skills {
        lines.push(field(
            "skills",
            match skills {
                Skills::Known(SkillsMode::Readonly) => vec![
                    Span::raw("read-only"),
                    Span::styled(
                        "   the host store, which it cannot change — K imports into it",
                        theme::muted(),
                    ),
                ],
                Skills::Known(SkillsMode::Readwrite) => vec![
                    Span::raw("read-write"),
                    Span::styled(
                        "   the host store, and it can rewrite what other sandboxes load",
                        theme::muted(),
                    ),
                ],
                Skills::Known(SkillsMode::Off) => vec![
                    Span::raw("not shared"),
                    Span::styled(
                        "   created with --skills=off; fixed at creation",
                        theme::muted(),
                    ),
                ],
                Skills::Unrecorded => vec![
                    Span::raw("shared"),
                    Span::styled(
                        "   mode not recorded — created outside aisbx",
                        theme::muted(),
                    ),
                ],
            },
        ));
    }

    let head = lines;
    let mut lines = Vec::new();
    lines.extend([
        field(
            "name",
            vec![Span::styled(sb.label.clone(), Style::default().bold())],
        ),
        field("sbx name", vec![Span::raw(sb.name.clone())]),
    ]);

    lines.push(field(
        "ssh",
        vec![
            Span::raw(crate::sbx::ssh_host(&sb.name)),
            Span::styled(
                match app.ssh_ready {
                    true => "   o connects an editor or a shell",
                    false => "   ⚠ run `sbx setup ssh` first — press o",
                },
                match app.ssh_ready {
                    true => theme::muted(),
                    false => theme::warn(),
                },
            ),
        ],
    ));

    match sb.created {
        Some(epoch) => lines.push(field(
            "created",
            vec![
                Span::raw(state::format_local(epoch)),
                Span::styled(format!("   {}", state::ago(epoch)), theme::muted()),
            ],
        )),
        None => lines.push(field("created", vec![Span::styled("—", theme::muted())])),
    }

    if let Some(epoch) = sb.last_used {
        lines.push(field(
            "last used",
            vec![
                Span::raw(state::format_local(epoch)),
                Span::styled(format!("   {}", state::ago(epoch)), theme::muted()),
            ],
        ));
    }

    if sb.cpus.is_some() || sb.memory.is_some() {
        let mut parts = Vec::new();
        if let Some(cpus) = sb.cpus {
            parts.push(format!("{cpus} CPU{}", if cpus == 1 { "" } else { "s" }));
        }
        if let Some(memory) = &sb.memory {
            parts.push(format!("{memory} memory"));
        }
        lines.push(field(
            "limits",
            vec![
                Span::raw(parts.join(" · ")),
                Span::styled("   recorded at creation; R keeps them", theme::muted()),
            ],
        ));
    }

    if let Some((value, gloss)) = sb.volumes_note() {
        lines.push(field(
            "volumes",
            vec![
                Span::raw(value),
                Span::styled(format!("   {gloss}"), theme::muted()),
            ],
        ));
    }

    lines.push(field(
        "location",
        vec![Span::styled(sb.location(), theme::muted())],
    ));

    if !sb.image.is_empty() {
        let mut spans = vec![Span::styled(sb.image.clone(), theme::muted())];
        if let Some(short) = sb.short_digest() {
            spans.push(Span::styled(format!("   {short}"), theme::muted()));
        }
        lines.push(field("image", spans));
    }

    if !sb.auth_mode.is_empty() {
        lines.push(field(
            "sbx auth",
            vec![Span::styled(sb.auth_mode.clone(), theme::muted())],
        ));
    }

    if !sb.secrets.is_empty() {
        let list: Vec<String> = sb
            .injected_secrets()
            .into_iter()
            .map(|(name, source)| match name {
                None => format!("placeholder only ({source})"),
                Some(name) => format!("{name} ({source})"),
            })
            .collect();
        lines.push(field(
            "secrets",
            vec![
                Span::styled(list.join("  "), theme::muted()),
                Span::styled("  — values stay on the host", theme::muted()),
            ],
        ));
    }

    if sb.mcp_gateway {
        let servers = app.registry.static_mcp(&sb.name);
        let mut spans = vec![Span::styled("gateway enabled", theme::muted())];
        if !servers.is_empty() {
            spans.push(Span::styled(
                format!("   {}", servers.join(" ")),
                theme::muted(),
            ));
        }
        lines.push(field("mcp", spans));
    }

    (head, lines)
}

/// Which Anthropic account the sandbox is on, and — where the answer is "not the one you think" —
/// what to press about it.
fn account_spans(auth: &Auth, users: usize) -> Vec<Span<'static>> {
    match auth {
        Auth::Login => vec![
            Span::raw("/login (OAuth)"),
            Span::styled(
                "   host-global — shared with every other /login sandbox",
                theme::muted(),
            ),
        ],
        Auth::Token {
            name,
            stored: false,
            ..
        } => vec![
            Span::styled(
                format!("setup token “{name}” — not stored"),
                theme::danger(),
            ),
            Span::styled(
                "   it cannot authenticate until one is stored: press A",
                theme::danger(),
            ),
        ],
        Auth::Token { name, .. } => vec![
            Span::styled(
                format!("setup token “{name}”"),
                Style::default().fg(theme::RUNNING),
            ),
            Span::styled(
                match users {
                    0 | 1 => {
                        "   independent of the /login; no other sandbox uses it yet".to_string()
                    }
                    2 => "   independent of the /login; 1 other sandbox uses it too".to_string(),
                    n => format!(
                        "   independent of the /login; {} other sandboxes use it too",
                        n - 1
                    ),
                },
                theme::muted(),
            ),
        ],
    }
}

/// The `⚑ network` row: where the waiting requests go — the first two, then how many more —
/// and the key that answers them. The destinations stand out bold.
fn waiting_spans(requests: &[&Approval]) -> Vec<Span<'static>> {
    let host =
        |a: &Approval| Span::styled(a.title.clone(), theme::warn().add_modifier(Modifier::BOLD));
    let word = |text: String| Span::styled(text, theme::warn());
    let mut spans = Vec::new();
    match requests {
        [] => return spans,
        [only] => spans.push(host(only)),
        [first, second] => spans.extend([host(first), word(" and ".into()), host(second)]),
        [first, second, rest @ ..] => spans.extend([
            host(first),
            word(", ".into()),
            host(second),
            word(format!(" and {} more", rest.len())),
        ]),
    }
    let verb = if requests.len() == 1 { "waits" } else { "wait" };
    spans.push(word(format!(" {verb} for approval — y to answer")));
    spans
}

fn status_spans(sb: &Sandbox) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!(
            "{} {}",
            if sb.running { "●" } else { "○" },
            sb.status_text()
        ),
        if sb.running {
            Style::default().fg(theme::RUNNING)
        } else {
            theme::muted()
        },
    )];
    if sb.unresponsive {
        spans.push(Span::styled(
            "   ⚠ not responding — s stops it, ↵ starts it again",
            theme::danger(),
        ));
    }
    if sb.sessions > 0 {
        spans.push(Span::styled(
            format!(
                "   {} open session{}",
                sb.sessions,
                if sb.sessions == 1 { "" } else { "s" }
            ),
            theme::muted(),
        ));
    }
    spans
}

fn resources(sb: &Sandbox, app: &App, width: usize) -> Vec<Line<'static>> {
    if !sb.running {
        return notes(&insight::resources_stopped_notes());
    }

    let cell = &app.insight.resources;
    let Some(sample) = &cell.value else {
        let mut lines = vec![note(insight::pending_text(
            Kind::Resources,
            cell.is_loading(),
        ))];
        if let Some(err) = &cell.error {
            lines.push(Line::from(Span::styled(err.clone(), theme::danger())));
        }
        return lines;
    };

    let bar_w = meter_width(width);
    let cpu = cpu_spans(sample.cpus, app.insight.cpu, bar_w);
    let memory = meter_spans(
        &format!(
            "{} used / {} total",
            stats::gb(sample.mem_used_kb()),
            stats::gb(sample.mem_total_kb)
        ),
        sample.mem_fraction(),
        bar_w,
    );
    let disk = meter_spans(
        &format!(
            "{} used / {} total",
            stats::gb(sample.disk_used_kb),
            stats::gb(sample.disk_total_kb)
        ),
        sample.disk_fraction(),
        bar_w,
    );
    let uptime = vec![Span::raw(stats::duration(sample.uptime_s as u64))];
    let column = [&cpu, &memory, &disk]
        .map(|s| spans_width(s))
        .into_iter()
        .max();
    let column = column.unwrap_or(0);
    let room = width.saturating_sub(LABEL_W + column + SPARK_GAP);
    let beside = (room >= SPARK_MIN).then(|| series(app, sb, room)).flatten();
    let mut lines = match beside {
        Some(series) => {
            let at = |mut spans: Vec<Span<'static>>, more: Vec<Span<'static>>| {
                let pad = column - spans_width(&spans).min(column) + SPARK_GAP;
                spans.push(Span::raw(" ".repeat(pad)));
                spans.extend(more);
                spans
            };
            let reach = format!("the last {}, newest at the right", series.reach);
            vec![
                field("CPUs", at(cpu, series.cpu)),
                field("Memory", at(memory, series.memory)),
                field("Disk", disk),
                field(
                    "Uptime",
                    at(uptime, vec![Span::styled(reach, theme::muted())]),
                ),
            ]
        }
        None => {
            let mut lines = vec![
                field("CPUs", cpu),
                field("Memory", memory),
                field("Disk", disk),
                field("Uptime", uptime),
            ];
            lines.extend(trend(app, sb, width));
            lines
        }
    };
    lines.push(Line::from(""));
    lines.push(note(insight::RESOURCES_SCOPE));
    if let Some(err) = &cell.error {
        lines.push(Line::from(Span::styled(
            insight::reading_failed(err),
            theme::warn(),
        )));
    }
    lines
}

/// The width of the label column [`field`] draws.
const LABEL_W: usize = 12;

/// The room between a meter's text and the sparkline beside it, and the fewest cells a sparkline
/// beside its meter takes; with less room the sparklines go under the meters.
const SPARK_GAP: usize = 3;
const SPARK_MIN: usize = 24;

/// A meter's width in a pane `width` wide: a quarter of the room after the labels, 18 to 40
/// cells.
fn meter_width(width: usize) -> usize {
    (width.saturating_sub(LABEL_W) / 4).clamp(18, 40)
}

/// How many cells `spans` take.
fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| s.width()).sum()
}

/// The readings since the tab opened as sparklines at most `room` cells wide, and how far back
/// they reach.
struct Series {
    cpu: Vec<Span<'static>>,
    memory: Vec<Span<'static>>,
    reach: String,
}

/// Nothing until there are two readings to compare.
fn series(app: &App, sb: &Sandbox, room: usize) -> Option<Series> {
    let points: Vec<stats::Point> = match app.histories.get(&sb.name) {
        Some(history) if history.len() > 1 => history.points().copied().collect(),
        _ => return None,
    };
    let room = room.max(1);
    let (cpu, shown) = spark(&points, |p| p.cpu, room);
    let (memory, _) = spark(&points, |p| Some(p.mem_fraction()), room);
    let first = points[points.len() - shown];
    let newest = points[points.len() - 1];
    let reach = stats::duration((newest.uptime_s - first.uptime_s).max(0.0) as u64);
    Some(Series { cpu, memory, reach })
}

/// The sparklines under the meters, the newest reading at the right, for a pane too narrow to
/// put them beside.
fn trend(app: &App, sb: &Sandbox, width: usize) -> Vec<Line<'static>> {
    let Some(series) = series(app, sb, width.saturating_sub(LABEL_W)) else {
        return Vec::new();
    };
    vec![
        Line::from(""),
        field("CPU %", series.cpu),
        field("Memory %", series.memory),
        field(
            "",
            vec![Span::styled(
                format!("the last {}, the newest reading at the right", series.reach),
                theme::muted(),
            )],
        ),
    ]
}

/// `points` as at most `width` cells, newest at the right: a block in its level's colour, or a
/// blank where the timeline broke or a point has no value. Returns the spans and the point count.
fn spark(
    points: &[stats::Point],
    value: impl Fn(&stats::Point) -> Option<f32>,
    width: usize,
) -> (Vec<Span<'static>>, usize) {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    // Cells from the newest back, each with the point it shows.
    let mut cells: Vec<Option<f32>> = Vec::new();
    let mut shown = 0;
    for p in points.iter().rev() {
        if cells.len() >= width {
            break;
        }
        cells.push(value(p));
        shown += 1;
        if p.gap_before && cells.len() < width {
            cells.push(None);
        }
    }
    cells.reverse();
    let mut spans: Vec<Span<'static>> = Vec::new();
    for cell in cells {
        let (glyph, style) = match cell {
            Some(f) => {
                let f = f.clamp(0.0, 1.0);
                let block = BLOCKS[(f * (BLOCKS.len() - 1) as f32).round() as usize];
                (block, Style::default().fg(level_color(f)))
            }
            None => (' ', Style::default()),
        };
        match spans.last_mut() {
            Some(last) if last.style == style => last.content.to_mut().push(glyph),
            _ => spans.push(Span::styled(glyph.to_string(), style)),
        }
    }
    (spans, shown)
}

/// A fraction's colour, by core's `stats::level`: the running green, the warning yellow, then the
/// danger red.
fn level_color(fraction: f32) -> ratatui::style::Color {
    match stats::level(fraction) {
        stats::Level::Critical => theme::DANGER,
        stats::Level::High => theme::WARN,
        stats::Level::Normal => theme::RUNNING,
    }
}

fn cpu_spans(cpus: usize, load: Option<f32>, bar_w: usize) -> Vec<Span<'static>> {
    let count = format!("{cpus} vCPU");
    match load {
        Some(load) => meter_spans(&count, load, bar_w),
        // The first reading has no interval to average over — a percentage needs two.
        None => vec![
            Span::raw(count),
            Span::styled(format!("   {}", insight::LOAD_NEEDS_SECOND), theme::muted()),
        ],
    }
}

fn meter_spans(text: &str, fraction: f32, bar_w: usize) -> Vec<Span<'static>> {
    let color = level_color(fraction);
    let mut spans = vec![
        Span::styled(bar(fraction, bar_w), Style::default().fg(color)),
        Span::styled(
            format!(" {:>3.0}%", fraction * 100.0),
            Style::default().fg(color),
        ),
    ];
    if !text.is_empty() {
        spans.push(Span::styled(format!("   {text}"), theme::muted()));
    }
    spans
}

/// A meter drawn to eighth-of-a-cell precision, so small values are still visible.
fn bar(fraction: f32, width: usize) -> String {
    const EIGHTHS: [char; 8] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
    let eighths = (fraction.clamp(0.0, 1.0) * (width * 8) as f32).round() as usize;
    let full = eighths / 8;
    let rest = eighths % 8;
    let mut out = String::with_capacity(width);
    out.extend(std::iter::repeat_n('█', full.min(width)));
    if full < width && rest > 0 {
        out.push(EIGHTHS[rest - 1]);
    }
    while out.chars().count() < width {
        out.push('░');
    }
    out
}

fn mounts(sb: &Sandbox) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = sb
        .mounts()
        .into_iter()
        .enumerate()
        .map(|(i, (path, mode))| {
            field(
                if i == 0 { "workspace" } else { "mount" },
                vec![
                    Span::raw(model::tilde(path)),
                    Span::styled(format!("   {mode}"), theme::muted()),
                ],
            )
        })
        .collect();

    let about = insight::mount_notes(sb);
    lines.push(Line::from(""));
    lines.extend(notes(&about.about));
    if let Some(skills) = sb.skills {
        lines.push(Line::from(""));
        lines.push(field(
            "skills",
            vec![
                Span::raw(insight::SKILLS_STORE),
                Span::styled(
                    format!("   {}", insight::skills_mount_text(skills)),
                    theme::muted(),
                ),
            ],
        ));
    }
    if !sb.runtime_mounts.is_empty() {
        lines.push(Line::from(""));
        for mount in &sb.runtime_mounts {
            let mut value = vec![Span::raw(model::tilde(Path::new(&mount.host_path)))];
            if mount.target() != mount.host_path {
                value.push(Span::styled(
                    format!(" → {}", mount.target()),
                    theme::muted(),
                ));
            }
            value.push(Span::styled(
                format!("   {}", insight::runtime_mount_access(mount)),
                theme::muted(),
            ));
            lines.push(field("added", value));
        }
    }
    lines.extend(about.added.iter().chain(&about.missing).map(note_line));
    lines
}

/// Host → sandbox port forwards.
fn ports(sb: &Sandbox, app: &App) -> Vec<Line<'static>> {
    let cell = &app.insight.ports;
    let mut lines = Vec::new();
    if let Some(err) = &cell.error {
        lines.push(Line::from(Span::styled(err.clone(), theme::danger())));
    }
    let Some(list) = &cell.value else {
        lines.push(note(insight::pending_text(Kind::Ports, cell.is_loading())));
        return lines;
    };

    for port in list {
        let mut spans = vec![
            Span::styled(
                format!("  {:<28}", port.url()),
                Style::default().fg(theme::RUNNING),
            ),
            Span::styled("→  sandbox ", theme::muted()),
            Span::raw(port.sandbox_port.to_string()),
            Span::styled(
                format!("  {}", insight::port_protocol(port)),
                theme::muted(),
            ),
        ];
        if port.ephemeral {
            spans.push(Span::styled(
                format!("  {}", insight::EPHEMERAL),
                theme::warn(),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines.extend(notes(&insight::port_notes(sb, list)));
    lines
}

fn network(sb: &Sandbox, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !sb.policy_scope.is_empty() {
        lines.push(field(
            "policy",
            vec![Span::raw(format!("{} scope", sb.policy_scope))],
        ));
    }
    if !sb.proxy.is_empty() {
        lines.push(field(
            "proxy",
            vec![
                Span::raw(sb.proxy.clone()),
                Span::styled(format!("   {}", insight::PROXY_GLOSS), theme::muted()),
            ],
        ));
    }
    if !sb.network.is_empty() {
        lines.push(field(
            "network",
            vec![Span::styled(sb.network.clone(), theme::muted())],
        ));
    }
    lines.push(Line::from(""));
    lines.extend(rule_lines(
        sb,
        &app.insight.network_rules(),
        app.insight.rules.is_loading(),
        app.insight.rules.error.as_deref(),
        insight::NO_NETWORK_RULES,
        width,
    ));

    if let Some(warning) = insight::network_warning(sb) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(warning, theme::danger())));
    }
    lines.push(Line::from(""));
    lines.push(note_line(&insight::network_check_note()));
    lines
}

fn filesystem(sb: &Sandbox, app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![note(insight::FILESYSTEM_ABOUT), Line::from("")];
    lines.extend(rule_lines(
        sb,
        &app.insight.filesystem_rules(),
        app.insight.rules.is_loading(),
        app.insight.rules.error.as_deref(),
        insight::NO_FILESYSTEM_RULES,
        width,
    ));
    lines
}

/// `rules` as the Network and Filesystem tabs list them: a line of columns per rule (decision,
/// type, source, reach), then what it matches, indented under them.
fn rule_lines(
    sb: &Sandbox,
    rules: &[&PolicyRule],
    loading: bool,
    error: Option<&str>,
    empty: &str,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(err) = error {
        lines.push(Line::from(Span::styled(err.to_string(), theme::danger())));
    }
    if rules.is_empty() {
        lines.push(note(match loading {
            true => insight::pending_text(Kind::Policy, true),
            false => empty,
        }));
        return lines;
    }
    for rule in rules {
        let (decision_style, marker) = if rule.allows() {
            (Style::default().fg(theme::RUNNING), "allow")
        } else {
            (theme::danger(), "deny ")
        };
        let mut tail = Vec::new();
        // Not muted: a rule of the host-wide policy is the one to notice.
        if let Some(reach) = insight::rule_reach(rule, &sb.name) {
            tail.push(Span::raw(reach));
        }
        if let Some(protocols) = insight::rule_protocols(rule) {
            tail.push(Span::styled(protocols, theme::muted()));
        }
        if let Some(status) = insight::rule_inactive(rule) {
            tail.push(Span::styled(format!("({status})"), theme::warn()));
        }
        let source = match tail.is_empty() {
            true => rule.source().to_string(),
            false => format!("{:<9}", rule.source()),
        };
        let mut header = vec![
            Span::styled(format!(" {marker} "), decision_style),
            Span::styled(format!("{:<16}", rule.resource_type), theme::muted()),
            Span::styled(source, Style::default().fg(origin_color(rule.source()))),
        ];
        for (i, span) in tail.into_iter().enumerate() {
            if i > 0 {
                header.push(Span::raw("   "));
            }
            header.push(span);
        }
        lines.push(Line::from(header));
        lines.extend(wrapped(&rule.targets(), 8, width));
    }
    lines
}

/// Lay `items` out across as many lines as they need, each indented by `indent`.
fn wrapped(items: &[String], indent: usize, width: usize) -> Vec<Line<'static>> {
    let usable = width.saturating_sub(indent).max(8);
    let mut lines = Vec::new();
    let mut current = String::new();
    for item in items {
        if !current.is_empty() && current.chars().count() + 2 + item.chars().count() > usable {
            lines.push(Line::from(Span::raw(format!(
                "{}{current}",
                " ".repeat(indent)
            ))));
            current.clear();
        }
        if !current.is_empty() {
            current.push_str("  ");
        }
        current.push_str(item);
    }
    if !current.is_empty() {
        lines.push(Line::from(Span::raw(format!(
            "{}{current}",
            " ".repeat(indent)
        ))));
    }
    lines
}

fn origin_color(source: &str) -> ratatui::style::Color {
    match insight::source_tone(source) {
        SourceTone::Accent => theme::ACCENT,
        SourceTone::Warn => theme::WARN,
        SourceTone::Quiet => theme::MUTED,
    }
}

fn activity(app: &App) -> Vec<Line<'static>> {
    let cell = &app.insight.log;
    let mut lines = Vec::new();
    if let Some(err) = &cell.error {
        lines.push(Line::from(Span::styled(err.clone(), theme::danger())));
    }
    let Some(log) = &cell.value else {
        lines.push(note(insight::pending_text(Kind::Log, cell.is_loading())));
        return lines;
    };
    if log.is_empty() {
        lines.push(note(insight::NO_REQUESTS));
        return lines;
    }

    if !log.blocked.is_empty() {
        lines.push(heading("blocked"));
        lines.extend(log.blocked.iter().map(|h| host_line(h, true)));
        if insight::asks_approval(log) {
            let mut line = note_line(&insight::approval_note());
            line.spans[0].content = format!("  {}", line.spans[0].content).into();
            lines.push(line);
        }
        lines.push(Line::from(""));
    }
    if !log.allowed.is_empty() {
        lines.push(heading("allowed"));
        lines.extend(log.allowed.iter().map(|h| host_line(h, false)));
    }
    lines
}

fn host_line(host: &LogHost, blocked: bool) -> Line<'static> {
    let style = if blocked {
        theme::danger()
    } else {
        Style::default().fg(theme::RUNNING)
    };
    let mut spans = vec![
        Span::styled(format!("  {:<38}", host.host), style),
        Span::styled(format!("{:<16}", host.proxy_type), theme::muted()),
        Span::styled(format!("×{:<6}", host.count), theme::muted()),
        Span::styled(short_time(&host.last_seen), theme::muted()),
    ];
    let why = insight::log_reason(host, blocked);
    if !why.is_empty() {
        spans.push(Span::styled(format!("   {why}"), theme::muted()));
    }
    Line::from(spans)
}

// --- shared -------------------------------------------------------------------------------

fn warnings(sb: &Sandbox) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(dir) = &sb.other_checkout {
        lines.push(field(
            "⚠ kits",
            vec![Span::styled(
                format!(
                    "matched by name — this sandbox points at {}. R rebuilds it from here.",
                    model::tilde(dir)
                ),
                theme::warn(),
            )],
        ));
    }
    if let Some(drift) = &sb.drift {
        lines.push(field(
            "⟳ update",
            vec![Span::styled(
                format!("{} Press R to rebuild.", drift.reason()),
                theme::warn().add_modifier(Modifier::BOLD),
            )],
        ));
    }
    lines
}

fn field(label: &str, mut value: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled(format!("{label:>10}  "), theme::muted())];
    spans.append(&mut value);
    Line::from(spans)
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        theme::muted().add_modifier(Modifier::BOLD),
    ))
}

fn note(text: &str) -> Line<'static> {
    Line::from(Span::styled(text.to_string(), theme::muted()))
}

/// A core note as one line: its words quiet, keys as keys, a blank note a blank line.
fn note_line(note: &Note) -> Line<'static> {
    if note.is_blank() {
        return Line::from("");
    }
    let span = |p: &insight::NotePiece| {
        let style = match p.kind {
            PieceKind::Plain => theme::muted(),
            PieceKind::Value => Style::default(),
            PieceKind::Key => theme::key(),
            PieceKind::Warn => theme::warn(),
            PieceKind::Danger => theme::danger(),
        };
        Span::styled(p.text.clone(), style)
    };
    Line::from(note.0.iter().map(span).collect::<Vec<_>>())
}

fn notes(notes: &[Note]) -> Vec<Line<'static>> {
    notes.iter().map(note_line).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::Sample;

    /// Two sandboxes, a short label and a long one; the first selected.
    fn labelled() -> App {
        let mut short = crate::model::sample("short", Some("claude-anthropic"), &[]);
        short.label = "api".into();
        let mut long = crate::model::sample("long", Some("opencode-litellm"), &[]);
        long.label = "OpenCode (Web, Host Gradle, Host Maven)".into();
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![short, long]);
        app.select(0);
        app
    }

    /// The pane's top border and the line under it, as drawn at `width`.
    fn top_lines(app: &mut App, width: u16) -> (String, String) {
        let screen = crate::ui::tests_support::render_at(app, width, 34);
        let lines: Vec<&str> = screen.lines().map(|l| l.trim_matches('"')).collect();
        let at = (lines.iter())
            .position(|l| l.starts_with("━━ 1 "))
            .unwrap_or_else(|| panic!("no tab strip:\n{screen}"));
        (lines[at].to_string(), lines[at + 1].to_string())
    }

    /// Each Overview row's label, in the order drawn.
    fn overview_labels(app: &App) -> Vec<String> {
        let sb = app.selected().expect("a sandbox is selected");
        (overview(sb, app).iter())
            .map(|line| line.spans[0].content.trim().to_string())
            .collect()
    }

    /// A wide pane puts the record beside what needs you, after a rule, the first rows side by
    /// side; a narrow one keeps one column in the same order.
    #[test]
    fn a_wide_overview_puts_the_record_in_a_second_column() {
        let mut sb = crate::model::sample("box", Some("claude-anthropic"), &["jdk25"]);
        sb.running = true;
        let app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb.clone()]);
        let one = text(&overview_in(&sb, &app, 80));
        assert_eq!(text(&overview(&sb, &app)), one);
        assert!(!one.iter().any(|l| l.contains('│')), "{one:?}");

        let (head, record) = overview_parts(&sb, &app);
        for width in [220, 150] {
            let two = text(&overview_in(&sb, &app, width));
            let rule = two[0].find('│').expect("a rule between the columns");
            for line in &two {
                assert!(line.chars().count() <= width, "{line}");
                let at = line.find('│').map(|b| line[..b].chars().count());
                let want = two[0][..rule].chars().count();
                assert_eq!(at, Some(want), "{width}: one rule column: {line}");
            }
            assert!(two[0].trim_start().starts_with("status"), "{}", two[0]);
            assert!(two[0].ends_with(&text(&record[..1])[0]), "{}", two[0]);
            let rows = head.len().max(record.len());
            match width {
                220 => assert_eq!(two.len(), rows, "{two:?}"),
                _ => {
                    let hung = |l: &&String| {
                        let left = &l[..l.find('│').unwrap()];
                        left.starts_with(&" ".repeat(12)) && !left.trim().is_empty()
                    };
                    assert!(two.iter().any(|l| hung(&l)), "a gloss wraps: {two:?}");
                }
            }
        }
        assert!(text(&overview_in(&sb, &app, 120))[0].find('│').is_none());
    }

    #[test]
    fn the_overview_leads_with_what_needs_you() {
        let mut sb = crate::model::sample("claude-litellm-api", Some("claude-litellm"), &["jdk25"]);
        sb.running = true;
        sb.sessions = 1;
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb]);
        app.approvals = vec![crate::ui::tests_support::held(
            "claude-litellm-api",
            "registry.npmjs.org:443",
        )];
        // A credential its kit declares and this host has not stored: no repo kit declares one
        // that is not optional, so the test's catalog does.
        let catalog = std::sync::Arc::make_mut(&mut app.catalog);
        let backend = (catalog.backends.iter_mut())
            .find(|b| b.kit.name == "claude-litellm")
            .expect("the gateway backend");
        backend.kit.credentials.push("example".into());
        app.secrets = Some(crate::sbx::Secrets::default());
        app.select(0);
        let labels = overview_labels(&app);
        let at = |label: &str| {
            (labels.iter())
                .position(|l| l == label)
                .unwrap_or_else(|| panic!("no {label}: {labels:?}"))
        };
        let order = [
            "status",
            "⚑ network",
            "⚠ secret",
            "agent",
            "kits",
            "folder",
            "skills",
            "name",
            "sbx name",
            "ssh",
            "created",
            "location",
            "image",
        ];
        assert_eq!(at("status"), 0, "{labels:?}");
        for pair in order.windows(2) {
            assert!(at(pair[0]) < at(pair[1]), "{pair:?}: {labels:?}");
        }
        app.approvals.clear();
        let screen = crate::ui::tests_support::render_at(&mut app, 80, 24);
        for row in ["status  ● running", "kits  jdk25"] {
            assert!(screen.contains(row), "{row} at 80×24:\n{screen}");
        }
    }

    #[test]
    fn the_network_row_names_the_waiting_destinations() {
        let held = |title: &str| crate::ui::tests_support::held("box", title);
        let text = |n: usize| {
            let requests: Vec<Approval> = (0..n).map(|i| held(&format!("h{i}:443"))).collect();
            let refs: Vec<&Approval> = requests.iter().collect();
            (waiting_spans(&refs).iter())
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(text(0), "");
        assert_eq!(text(1), "h0:443 waits for approval — y to answer");
        assert_eq!(text(2), "h0:443 and h1:443 wait for approval — y to answer");
        assert_eq!(
            text(3),
            "h0:443, h1:443 and 1 more wait for approval — y to answer"
        );
        assert_eq!(
            text(5),
            "h0:443, h1:443 and 3 more wait for approval — y to answer"
        );
    }

    #[test]
    fn the_tabs_are_the_panes_top_border() {
        let mut app = labelled();
        let (border, under) = top_lines(&mut app, 110);
        for tab in Tab::ALL {
            assert!(border.contains(tab.title()), "{tab:?}: {border}");
        }
        assert!(border.ends_with("━━ api ━━"), "{border}");
        assert!(under.trim_start().starts_with("status  "), "{under}");
        let y = (app.hits.iter())
            .find(|(_, hit)| *hit == Hit::TabAt(0))
            .map(|(r, _)| r.y)
            .expect("a click target per tab");
        let tabs: Vec<_> = (app.hits.iter())
            .filter(|(_, hit)| matches!(hit, Hit::TabAt(_)))
            .collect();
        assert_eq!(tabs.len(), Tab::ALL.len());
        assert!(tabs.iter().all(|(r, _)| r.y == y), "{tabs:?}");
        let network = (tabs.iter())
            .find(|(_, hit)| *hit == Hit::TabAt(Tab::Network.index()))
            .unwrap()
            .0;
        app.on_mouse(crate::input::MouseEvent {
            kind: crate::input::MouseEventKind::Down(crate::input::MouseButton::Left),
            column: network.x + 1,
            row: network.y,
            modifiers: crate::input::KeyModifiers::NONE,
        });
        assert_eq!(app.tab, Tab::Network);
    }

    #[test]
    fn the_tabs_stay_put_and_the_label_gets_what_they_leave() {
        for width in [110, 96, 88, 80, 60] {
            let mut app = labelled();
            let tabs = |app: &App| -> Vec<(u16, u16)> {
                (app.hits.iter())
                    .filter(|(_, hit)| matches!(hit, Hit::TabAt(_)))
                    .map(|(r, _)| (r.x, r.width))
                    .collect()
            };
            top_lines(&mut app, width);
            let short = tabs(&app);
            app.select(1);
            let (border, _) = top_lines(&mut app, width);
            assert_eq!(tabs(&app), short, "{width}: the tabs moved");
            assert!(border.chars().count() == width as usize, "{border}");
            let label = match width {
                110 => Some("OpenCode (Web, Host G…"),
                96 => Some("OpenCod…"),
                88 => None,
                80 => Some("OpenCode (Web, Host Gradle, Host Maven)"),
                _ => Some("OpenCode (Web, Host Gr…"),
            };
            match label {
                Some(label) => {
                    let end = format!(" {label} ━━");
                    assert!(border.ends_with(&end), "{width}: {border}");
                }
                None => assert!(!border.contains("OpenCo"), "{width}: {border}"),
            }
            let full = Tab::ALL.iter().all(|t| border.contains(t.title()));
            assert_eq!(full, width >= 88, "{width}: {border}");
        }
    }

    #[test]
    fn the_meter_shows_small_values_without_rounding_them_away() {
        assert_eq!(bar(0.0, 4).chars().count(), 4);
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██░░");
        // A fraction well under one cell is still a visible sliver, not nothing.
        assert!(bar(0.005, 20).starts_with('▏'));
        assert!(bar(0.02, 20).starts_with('▍'));
        assert_eq!(bar(2.0, 3), "███");
    }

    /// The Overview's ledger in one column.
    fn overview(sb: &Sandbox, app: &App) -> Vec<Line<'static>> {
        let (mut lines, record) = overview_parts(sb, app);
        lines.extend(record);
        lines
    }

    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn a_wrapped_value_hangs_under_the_value_column() {
        let line = field(
            "agent",
            vec![
                Span::raw("Claude Code"),
                Span::styled("  — your own Anthropic account", theme::muted()),
            ],
        );
        let lines = hang(vec![line], 30);
        assert_eq!(
            text(&lines),
            [
                "     agent  Claude Code  —",
                "            your own Anthropic",
                "            account",
            ]
        );
        assert!(lines.iter().all(|l| l.width() <= 30));
        let last = lines.last().unwrap();
        assert_eq!(last.spans.last().unwrap().style, theme::muted());
    }

    #[test]
    fn wrapping_keeps_short_lines_indents_and_long_words() {
        let short = Line::from("  fits");
        assert_eq!(text(&hang(vec![short], 10)), ["  fits"]);
        let indented = Line::from("   one two three four");
        assert_eq!(
            text(&hang(vec![indented], 12)),
            ["   one two", "   three", "   four"]
        );
        let long = Line::from("abcdefghijkl");
        assert_eq!(text(&hang(vec![long], 5)), ["abcde", "fghij", "kl"]);
    }

    #[test]
    fn the_scroll_hint_counts_hidden_lines() {
        let none = Hidden { above: 0, below: 0 };
        assert_eq!(scroll_hint(none, false), None);
        let below = Hidden { above: 0, below: 4 };
        let hint = scroll_hint(below, false).unwrap().to_string();
        assert_eq!(hint, " ↓ 4 more  ctrl-↑↓ scroll ");
        let both = Hidden { above: 2, below: 1 };
        let hint = scroll_hint(both, true).unwrap().to_string();
        assert_eq!(hint, " ↑ 2 ↓ 1 more  ↑↓ scroll ");
    }

    #[test]
    fn a_narrow_tab_strip_keeps_every_tab() {
        let sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb]);
        app.select_tab(4);
        let screen = crate::ui::tests_support::render_at(&mut app, 50, 30);
        let strip = screen
            .lines()
            .find(|l| l.contains("Network"))
            .expect("the open tab is named");
        for key in ["1", "2", "3", "4", "6", "7"] {
            assert!(strip.contains(&format!(" {key} ")), "{key} in {strip}");
        }
        assert!(!strip.contains("Overview"), "{strip}");
        let wide = crate::ui::tests_support::render_at(&mut app, 120, 30);
        assert!(wide.contains("1 Overview") && wide.contains("7 Activity"));
    }

    #[test]
    fn scrolling_reaches_the_last_wrapped_line() {
        let sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb]);
        let screen = crate::ui::tests_support::render_at(&mut app, 50, 24);
        assert!(screen.contains("more  ctrl-↑↓ scroll"), "{screen}");
        let mut seen = String::new();
        for scroll in 0..40 {
            app.detail_scroll = scroll;
            seen.push_str(&crate::ui::tests_support::render_at(&mut app, 50, 24));
        }
        // The agent row's gloss ends past column 50, and the last row is the sbx auth mode.
        assert!(seen.contains("Enterprise)"), "{seen}");
        assert!(seen.contains("sbx auth"), "{seen}");
        app.detail_scroll = u16::MAX;
        let screen = crate::ui::tests_support::render_at(&mut app, 50, 24);
        assert!(!screen.contains("more  ctrl"), "{screen}");
        assert!(screen.contains("↑ "), "{screen}");
    }

    #[test]
    fn timestamps_shrink_to_the_clock_time() {
        assert_eq!(short_time("2026-07-24T10:39:45.030935+02:00"), "10:39:45");
        assert_eq!(short_time("nonsense"), "nonsense");
    }

    #[test]
    fn resource_sample_is_only_needed_when_running() {
        let mut sb = crate::model::sample("s", Some("claude-anthropic"), &[]);
        let app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb.clone()]);
        assert!(resources(&sb, &app, 80)
            .iter()
            .any(|l| l.to_string().contains("stopped")));
        sb.running = true;
        assert!(resources(&sb, &app, 80)
            .iter()
            .any(|l| l.to_string().contains("measuring")));
    }

    #[test]
    fn a_sparkline_draws_a_cell_per_reading_the_newest_last() {
        let point = |cpu: Option<f32>, gap_before| stats::Point {
            uptime_s: 0.0,
            cpu,
            mem_used_kb: 1,
            mem_total_kb: 2,
            gap_before,
        };
        let points = [
            point(None, false),
            point(Some(0.0), false),
            point(Some(0.5), false),
            point(Some(1.0), true),
            point(Some(0.75), false),
        ];
        let joined = |spans: &[Span]| {
            spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        let (spans, shown) = spark(&points, |p| p.cpu, 10);
        assert_eq!(joined(&spans), " ▁▅ █▆");
        assert_eq!(shown, 5);
        let colour = |glyph: char| {
            spans
                .iter()
                .find(|s| s.content.contains(glyph))
                .unwrap()
                .style
                .fg
        };
        assert_eq!(colour('▁'), Some(theme::RUNNING));
        assert_eq!(colour('▅'), Some(theme::RUNNING));
        assert_eq!(colour('▆'), Some(theme::WARN));
        assert_eq!(colour('█'), Some(theme::DANGER));
        assert_eq!(spans.len(), 5, "a run of one colour is one span");
        let (spans, shown) = spark(&points, |p| p.cpu, 2);
        assert_eq!((joined(&spans).as_str(), shown), ("█▆", 2));
        let (spans, _) = spark(&points, |p| Some(p.mem_fraction()), 10);
        assert_eq!(joined(&spans), "▅▅▅ ▅▅", "the break shows on every line");
    }

    #[test]
    fn the_resources_tab_draws_the_trend_once_two_readings_are_in() {
        let sb = running(&[]);
        let mut app = sampled(&sb);
        let probe = |total: u64, idle: u64, uptime: f64| {
            Sample::parse(&format!(
                "cpu  {} 0 0 {idle} 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\n\
                 MemAvailable: 524288 kB\n{uptime} 100.0\nx 1000 500 500 50% /\n",
                total - idle
            ))
            .unwrap()
        };
        let row = |lines: &[String], label: &str| {
            let start = format!("{label:>10}  ");
            lines.iter().find(|l| l.starts_with(&start)).cloned()
        };
        app.histories.record(&sb.name, &probe(1000, 900, 100.0));
        let one = text(&resources(&sb, &app, 80));
        assert_eq!(row(&one, "CPU %"), None, "{one:?}");
        app.histories.record(&sb.name, &probe(2000, 1400, 105.0));
        let two = text(&resources(&sb, &app, 80));
        assert_eq!(row(&two, "CPU %").as_deref(), Some("     CPU %   ▅"));
        assert_eq!(row(&two, "Memory %").as_deref(), Some("  Memory %  ▅▅"));
        assert!(
            two.iter()
                .any(|l| l.contains("the last 5s, the newest reading at the right")),
            "{two:?}"
        );
        let meters = two
            .iter()
            .position(|l| l.starts_with("    Uptime"))
            .unwrap();
        let trend = two
            .iter()
            .position(|l| l.starts_with("     CPU %  "))
            .unwrap();
        assert!(meters < trend, "under the meters");
    }

    /// On a wide pane the meters grow and each sparkline stands beside its meter, all starting
    /// in one column; at 80 columns they stay under the meters.
    #[test]
    fn a_wide_pane_puts_the_sparklines_beside_their_meters() {
        let sb = running(&[]);
        let mut app = sampled(&sb);
        let probe = |total: u64, idle: u64, uptime: f64| {
            Sample::parse(&format!(
                "cpu  {} 0 0 {idle} 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\n\
                 MemAvailable: 524288 kB\n{uptime} 100.0\nx 1000 500 500 50% /\n",
                total - idle
            ))
            .unwrap()
        };
        app.histories.record(&sb.name, &probe(1000, 900, 100.0));
        app.histories.record(&sb.name, &probe(2000, 1400, 105.0));
        let wide = text(&resources(&sb, &app, 140));
        assert!(!wide.iter().any(|l| l.contains("CPU %")), "{wide:?}");
        let row = |label: &str| {
            let start = format!("{label:>10}  ");
            wide.iter()
                .find(|l| l.starts_with(&start))
                .cloned()
                .unwrap()
        };
        let (cpu, memory, uptime) = (row("CPUs"), row("Memory"), row("Uptime"));
        let bar: usize = cpu.chars().filter(|c| "█▏▎▍▌▋▊▉░".contains(*c)).count();
        assert_eq!(bar, meter_width(140), "{cpu}");
        assert!(bar > 18);
        let column = |line: &str, glyph: char| line.chars().position(|c| c == glyph);
        assert!(
            cpu.ends_with('▅') && memory.ends_with("▅▅"),
            "{cpu}\n{memory}"
        );
        let reach = "the last 5s, newest at the right";
        assert!(uptime.ends_with(reach), "{uptime}");
        let spark_at = column(&memory, '▅').unwrap();
        let reach_at = uptime.find(reach).map(|b| uptime[..b].chars().count());
        assert_eq!(reach_at, Some(spark_at), "{memory}\n{uptime}");
        let first = cpu.chars().nth(spark_at);
        assert_eq!(first, Some(' '), "the first reading has no CPU %: {cpu}");
        assert_eq!(cpu.chars().count(), spark_at + 2, "{cpu}");
        for line in &wide {
            assert!(line.chars().count() <= 140, "{line}");
        }
    }

    #[test]
    fn meters_and_sparklines_have_ascii_stand_ins() {
        let mut drawn: Vec<char> = (0..=160)
            .flat_map(|i| bar(i as f32 / 160.0, 20).chars().collect::<Vec<_>>())
            .collect();
        let point = |cpu: f32| stats::Point {
            uptime_s: 0.0,
            cpu: Some(cpu),
            mem_used_kb: 1,
            mem_total_kb: 2,
            gap_before: false,
        };
        let points: Vec<stats::Point> = (0..=7).map(|i| point(i as f32 / 7.0)).collect();
        let (spans, _) = spark(&points, |p| p.cpu, 8);
        let line: String = spans.iter().map(|s| s.content.to_string()).collect();
        drawn.extend(line.chars());
        for c in drawn {
            assert_ne!(theme::ascii(&c.to_string()), Some("?"), "{c:?}");
        }
        let plain: String = (line.chars())
            .map(|c| theme::ascii(&c.to_string()).unwrap_or("?").to_string())
            .collect();
        assert_eq!(plain, "__.--=##");
    }

    #[test]
    fn sample_formats_into_the_expected_rows() {
        let sample = Sample::parse(
            "cpu  10 0 10 80 0\ncpu0 1 0 1 8 0\nMemTotal: 1048576 kB\nMemAvailable: 524288 kB\n100.0 100.0\nx 1000 500 500 50% /\n",
        )
        .unwrap();
        assert_eq!(stats::gb(sample.mem_total_kb), "1.0 GB");
        assert_eq!(sample.mem_fraction(), 0.5);
        assert_eq!(sample.disk_fraction(), 0.5);
    }

    // --- pinned tabs: every span's text and style, as drawn today ---------------------------

    /// One line per drawn line, each span as `text` or `text‹style›` — the style named after the
    /// theme helper that made it, so a moved rule that changes a colour shows up here too.
    fn pin(lines: &[Line]) -> String {
        let name = |style: Style| {
            let running = Style::default().fg(theme::RUNNING);
            let named = [
                (Style::default(), ""),
                (theme::muted(), "muted"),
                (theme::key(), "key"),
                (theme::danger(), "danger"),
                (theme::warn(), "warn"),
                (running, "running"),
                (Style::default().fg(theme::DANGER), "red"),
                (Style::default().fg(theme::WARN), "yellow"),
                (Style::default().fg(theme::ACCENT), "accent"),
                (Style::default().fg(theme::MUTED), "grey"),
                (theme::muted().add_modifier(Modifier::BOLD), "heading"),
            ];
            match named.iter().find(|(s, _)| *s == style) {
                Some((_, "")) => String::new(),
                Some((_, n)) => format!("‹{n}›"),
                None => format!("‹{style:?}›"),
            }
        };
        let line = |l: &Line| {
            let spans: Vec<String> = l
                .spans
                .iter()
                .map(|s| format!("{}{}", s.content, name(s.style)))
                .collect();
            spans.join("")
        };
        lines.iter().map(line).collect::<Vec<_>>().join("\n")
    }

    /// An app on `sb` with `Insight::sample`'s readings, rules, ports and log.
    fn sampled(sb: &Sandbox) -> App {
        let mut app = App::for_test(crate::kits::Catalog::load().unwrap(), vec![sb.clone()]);
        app.insight = crate::insight::Insight::sample(&sb.name);
        app
    }

    fn running(addons: &[&str]) -> Sandbox {
        let mut sb = crate::model::sample("s", Some("claude-anthropic"), addons);
        sb.running = true;
        sb
    }

    #[test]
    fn pinned_resources_running_and_stopped() {
        let sb = running(&[]);
        let app = sampled(&sb);
        assert_eq!(pin(&resources(&sb, &app, 80)), PIN_RESOURCES_RUNNING);
        let mut stopped = sb.clone();
        stopped.running = false;
        assert_eq!(pin(&resources(&stopped, &app, 80)), PIN_RESOURCES_STOPPED);
        let mut fresh = sampled(&sb);
        fresh.insight.cpu = None;
        assert_eq!(pin(&resources(&sb, &fresh, 80)), PIN_RESOURCES_FIRST);
        let meters: Vec<Line> = [0.69, 0.7, 0.89, 0.9]
            .map(|f| Line::from(meter_spans("", f, 18)))
            .into();
        assert_eq!(pin(&meters), PIN_METERS);
    }

    #[test]
    fn pinned_mounts_bind_clone_and_mountless() {
        let mut bind = running(&["host-gradle"]);
        bind.ro_mounts = vec!["/home/u/.gradle".into()];
        assert_eq!(pin(&mounts(&bind)), PIN_MOUNTS_BIND);
        let mut clone = running(&[]);
        clone.clone = true;
        clone.workspace_missing = true;
        clone.skills = Some(Skills::Known(SkillsMode::Readwrite));
        assert_eq!(pin(&mounts(&clone)), PIN_MOUNTS_CLONE);
        let mut mountless = running(&[]);
        mountless.workspace = Default::default();
        mountless.skills = Some(Skills::Unrecorded);
        mountless.runtime_mounts = vec![
            crate::sbx::RuntimeMount {
                host_path: "/data/set".into(),
                container_target: "/mnt/set".into(),
                read_only: true,
            },
            crate::sbx::RuntimeMount {
                host_path: "/data/out".into(),
                container_target: String::new(),
                read_only: false,
            },
        ];
        assert_eq!(pin(&mounts(&mountless)), PIN_MOUNTS_MOUNTLESS);
    }

    #[test]
    fn pinned_ports_listed_empty_and_stopped() {
        let sb = running(&[]);
        let app = sampled(&sb);
        assert_eq!(pin(&ports(&sb, &app)), PIN_PORTS_LISTED);
        let mut kept = sb.clone();
        kept.keep_running = true;
        assert_eq!(pin(&ports(&kept, &app)), PIN_PORTS_KEPT);
        let mut loading = sampled(&sb);
        loading.insight.ports = Default::default();
        assert_eq!(pin(&ports(&sb, &loading)), PIN_PORTS_LOADING);
        loading.insight.ports.value = Some(Vec::new());
        assert_eq!(pin(&ports(&sb, &loading)), PIN_PORTS_EMPTY);
        let mut stopped = sb.clone();
        stopped.running = false;
        assert_eq!(pin(&ports(&stopped, &app)), PIN_PORTS_STOPPED);
    }

    #[test]
    fn pinned_network_filesystem_and_activity() {
        let mut sb = running(&["web"]);
        sb.proxy = "http://gateway.docker.internal:3128".into();
        sb.network = "sbx-net".into();
        let mut app = sampled(&sb);
        let rules = app.insight.rules.value.as_mut().unwrap();
        for (name, actions, status) in [
            ("both", vec!["net:connect:tcp", "net:connect:udp"], "active"),
            ("dns", vec!["net:connect:udp"], "inactive"),
        ] {
            rules.push(PolicyRule {
                name: name.into(),
                scope: "global".into(),
                applies_to: "all".into(),
                resource_type: "network".into(),
                decision: "deny".into(),
                resources: vec![format!("{name}.example.com")],
                status: status.into(),
                actions: actions.into_iter().map(String::from).collect(),
                ..Default::default()
            });
        }
        let log = app.insight.log.value.as_mut().unwrap();
        log.blocked.push(LogHost {
            host: "evil.example.com:443".into(),
            proxy_type: "forward".into(),
            rule: "deny-evil".into(),
            reason: String::new(),
            last_seen: "2026-07-24T11:00:01Z".into(),
            count: 1,
        });
        assert_eq!(pin(&network(&sb, &app, 60)), PIN_NETWORK);
        assert_eq!(pin(&filesystem(&sb, &app, 60)), PIN_FILESYSTEM);
        assert_eq!(pin(&activity(&app)), PIN_ACTIVITY);
    }

    const PIN_RESOURCES_RUNNING: &str = "      CPUs  ‹muted›██▏░░░░░░░░░░░░░░░‹running›  12%‹running›   2 vCPU‹muted›\n    Memory  ‹muted›▍░░░░░░░░░░░░░░░░░‹running›   2%‹running›   0.3 GB used / 15.8 GB total‹muted›\n      Disk  ‹muted›▎░░░░░░░░░░░░░░░░░‹running›   2%‹running›   0.3 GB used / 19.5 GB total‹muted›\n    Uptime  ‹muted›8m 25s\n\nThe sandbox microVM's own CPUs, memory and root filesystem — not the host's.‹muted›";
    const PIN_RESOURCES_STOPPED: &str = "The sandbox is stopped.‹muted›\n\nCPU, memory and disk are read from inside the sandbox, which needs it to be‹muted›\nrunning — starting one just to measure it would be worse than saying so.‹muted›\n\n↵‹key›  start it, or  ‹muted›x‹key›  open a shell inside it‹muted›";
    const PIN_RESOURCES_FIRST: &str = "      CPUs  ‹muted›2 vCPU   load needs a second reading‹muted›\n    Memory  ‹muted›▍░░░░░░░░░░░░░░░░░‹running›   2%‹running›   0.3 GB used / 15.8 GB total‹muted›\n      Disk  ‹muted›▎░░░░░░░░░░░░░░░░░‹running›   2%‹running›   0.3 GB used / 19.5 GB total‹muted›\n    Uptime  ‹muted›8m 25s\n\nThe sandbox microVM's own CPUs, memory and root filesystem — not the host's.‹muted›";
    const PIN_METERS: &str = "████████████▍░░░░░‹running›  69%‹running›\n████████████▋░░░░░‹warn›  70%‹warn›\n████████████████░░‹warn›  89%‹warn›\n████████████████▎░‹red›  90%‹red›";
    const PIN_MOUNTS_BIND: &str = " workspace  ‹muted›/proj   read-write‹muted›\n     mount  ‹muted›/home/u/.gradle   read-only‹muted›\n\nThe workspace is bind-mounted: edits inside the sandbox change these files.‹muted›\n\n    skills  ‹muted›the host's shared agent-skills store   read-only, linked into the agent's skills folder‹muted›";
    const PIN_MOUNTS_CLONE: &str = " workspace  ‹muted›/proj   read-only (clone mode)‹muted›\n\nClone mode: the agent works on a private in-container clone. Fetch its commits‹muted›\nwith  git fetch sandbox-s‹muted›\nNo extra mounts. The host-gradle and host-maven kits add read-only cache mounts.‹muted›\n\n    skills  ‹muted›the host's shared agent-skills store   read-write, mounted over the agent's skills folder‹muted›\n⚠ the workspace folder no longer exists on this host‹danger›";
    const PIN_MOUNTS_MOUNTLESS: &str = "\nNo host folder is mounted (a mountless sandbox): the agent works in /home/agent/workspace,‹muted›\ninside the sandbox. Those files survive a stop, and are deleted with the sandbox —‹muted›\n`sbx cp` copies them out (sandbox paths must be absolute).‹muted›\nNo extra mounts. The host-gradle and host-maven kits add read-only cache mounts.‹muted›\n\n    skills  ‹muted›the host's shared agent-skills store   mode not recorded‹muted›\n\n     added  ‹muted›/data/set → /mnt/set‹muted›   read-only‹muted›\n     added  ‹muted›/data/out   read-write‹muted›\nAdded with sbx mount after creation; restored on every start, and mounted again by R.‹muted›";
    const PIN_PORTS_LISTED: &str = "  http://127.0.0.1:8080       ‹running›→  sandbox ‹muted›3000  tcp‹muted›\n  http://127.0.0.1:54321      ‹running›→  sandbox ‹muted›8000  tcp‹muted›  ephemeral‹warn›\n\np‹key›  publish another   ‹muted›u‹key›  unpublish‹muted›\n\nThe service inside must listen on 0.0.0.0 (or [::]), not 127.0.0.1, to be reachable.‹muted›\nIt stops itself shortly after its last session ends, and these stop answering with it‹muted›\n— ‹muted›b‹key› keeps it running in the background.‹muted›";
    const PIN_PORTS_KEPT: &str = "  http://127.0.0.1:8080       ‹running›→  sandbox ‹muted›3000  tcp‹muted›\n  http://127.0.0.1:54321      ‹running›→  sandbox ‹muted›8000  tcp‹muted›  ephemeral‹warn›\n\np‹key›  publish another   ‹muted›u‹key›  unpublish‹muted›\n\nThe service inside must listen on 0.0.0.0 (or [::]), not 127.0.0.1, to be reachable.‹muted›";
    const PIN_PORTS_LOADING: &str = "loading…‹muted›";
    const PIN_PORTS_EMPTY: &str = "No ports published — nothing on the host can reach into this sandbox.‹muted›\n\np‹key›  publish one, e.g.  ‹muted›8080:3000  (host 8080 → sandbox 3000)‹muted›\n\nThe service inside must listen on 0.0.0.0 (or [::]), not 127.0.0.1, to be reachable.‹muted›";
    const PIN_PORTS_STOPPED: &str = "  http://127.0.0.1:8080       ‹running›→  sandbox ‹muted›3000  tcp‹muted›\n  http://127.0.0.1:54321      ‹running›→  sandbox ‹muted›8000  tcp‹muted›  ephemeral‹warn›\n\np‹key›  publish another   ‹muted›u‹key›  unpublish‹muted›\n\nThe service inside must listen on 0.0.0.0 (or [::]), not 127.0.0.1, to be reachable.‹muted›\nThe sandbox is stopped. Published ports are restored when it starts again;‹muted›\npublishing a new one starts it first.‹muted›\nIt stops itself shortly after its last session ends, and these stop answering with it‹muted›\n— ‹muted›b‹key› keeps it running in the background.‹muted›";
    const PIN_NETWORK: &str = "    policy  ‹muted›global scope\n     proxy  ‹muted›http://gateway.docker.internal:3128   enforces the rules below‹muted›\n   network  ‹muted›sbx-net‹muted›\n\n deny  ‹danger›http            ‹muted›added‹muted›\n        POST api.github.com/repos/**?state=open\n deny  ‹danger›network         ‹muted›local    ‹muted›all sandboxes   tcp+udp‹muted›\n        both.example.com\n deny  ‹danger›network         ‹muted›local    ‹muted›all sandboxes   udp‹muted›   (inactive)‹warn›\n        dns.example.com\n allow ‹running›network         ‹muted›approved‹warn›\n        registry.npmjs.org:443\n allow ‹running›network         ‹muted›kit‹accent›\n        api.anthropic.com  console.anthropic.com  claude.ai\n        platform.claude.com  downloads.claude.ai:443\n        github.com  *.githubusercontent.com\n\n⚠ the web kit allows ** — every host is reachable from this sandbox‹danger›\n\nc‹key›  check whether a host is reachable from this sandbox‹muted›";
    const PIN_FILESYSTEM: &str = "Which host paths any sandbox may be given as a workspace.‹muted›\n\n allow ‹running›filesystem:read ‹muted›local    ‹muted›all sandboxes\n        **";
    const PIN_ACTIVITY: &str = "blocked‹heading›\n  registry.example.com:443              ‹danger›forward         ‹muted›×3     ‹muted›10:39:45‹muted›   no rule (default deny)‹muted›\n  evil.example.com:443                  ‹danger›forward         ‹muted›×1     ‹muted›11:00:01‹muted›   deny-evil‹muted›\n  no rule allowed those — the sandbox asked for approval: ‹muted›y‹key› answers it‹muted›\n\nallowed‹heading›\n  api.anthropic.com:443                 ‹running›forward         ‹muted›×128   ‹muted›10:39:45‹muted›\n  github.com:443                        ‹running›forward-bypass  ‹muted›×4     ‹muted›10:39:45‹muted›";
}

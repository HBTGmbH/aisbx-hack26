//! The sandbox list: one row per sandbox, grouped by folder.
//! Core decides the rows and their text ([`crate::roster`]); this module lays them out in columns.

use std::ops::Range;

use crate::input::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{clip, panel, Hits};
use crate::app::{App, Hit};
use crate::find;
use crate::model::{CardMarker, CardNote, Sandbox};
use crate::roster::{self, FolderHead, Row};
use crate::theme;

/// Columns before a row's name: the selection bar, the status dot and a space.
const LEAD: u16 = 3;
/// Blank columns between two columns of a row.
const GAP: u16 = 2;
/// Below this inner width the middle column moves under the name.
const NARROW: u16 = 72;

/// Where a row's columns start and how wide they are — the same for every row of the list, so
/// the columns line up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Columns {
    /// The inner width of the panel.
    width: u16,
    /// Start and width of the name, runs and state columns.
    name: (u16, u16),
    runs: (u16, u16),
    state: (u16, u16),
    /// The runs column is folded under the name: the agent is the second line, and the start
    /// folder and the kits are left to the details pane.
    narrow: bool,
}

impl Columns {
    /// Sized by what the widest entries of *every* sandbox need, drawn or not: the columns must
    /// not move when the list scrolls or the second section folds.
    fn new(app: &App, width: u16) -> Self {
        // The middle column's need is the agent: the kits count what they cannot show.
        let mut want = [0usize; 3];
        for sb in &app.sandboxes {
            let waiting = app.approvals_for(&sb.name);
            want[0] = want[0]
                .max(sb.label.chars().count())
                .max(sb.start_inside().map_or(0, |s| s.chars().count()));
            want[1] = want[1].max(sb.type_label(&app.catalog).chars().count());
            want[2] = want[2]
                .max(sb.status_parts().join(" · ").chars().count())
                .max(attention_width(sb, waiting));
        }

        if width < NARROW {
            let state = (want[2] as u16).min(width * 2 / 5);
            let name = width.saturating_sub(LEAD + GAP + state);
            return Columns {
                width,
                name: (LEAD, name),
                runs: (LEAD, 0),
                state: (width.saturating_sub(state), state),
                narrow: true,
            };
        }
        let avail = width.saturating_sub(LEAD + 2 * GAP);
        let [name, _, state] = roster::columns(avail as f32, want.map(|w| w as f32));
        let (name, state) = (name as u16, state as u16);
        // The rounding remainder goes to the middle, which ends one gap before the state.
        let runs = avail.saturating_sub(name + state);
        Columns {
            width,
            name: (LEAD, name),
            runs: (LEAD + name + GAP, runs),
            state: (width.saturating_sub(state), state),
            narrow: false,
        }
    }

    /// Lines a sandbox's row takes: a second one only for something to put on it — and always in
    /// the narrow layout, whose second line is the agent.
    fn lines(&self, sb: &Sandbox, waiting: usize) -> u16 {
        if self.narrow || sb.second_line(waiting) {
            2
        } else {
            1
        }
    }
}

/// How many lines a row of the list takes.
fn height(app: &App, row: &Row, cols: &Columns) -> u16 {
    match row {
        Row::Other | Row::Folder(_) => 2,
        Row::Sandbox(cell) => app
            .sandbox_at(*cell)
            .map_or(1, |sb| cols.lines(sb, app.approvals_for(&sb.name))),
        Row::Here | Row::New(_) => 1,
    }
}

/// How many lines the list's panel wants, borders included. Every sandbox counts two lines, with
/// or without a second line now, so the split under the list stays put when a note comes or goes.
pub(super) fn wanted_height(app: &App) -> u16 {
    let lines: usize = app
        .roster()
        .iter()
        .map(|row| match row {
            Row::Other | Row::Folder(_) | Row::Sandbox(_) => 2,
            Row::Here | Row::New(_) => 1,
        })
        .sum();
    u16::try_from(lines + 2).unwrap_or(u16::MAX)
}

/// The row a cell is drawn in.
fn row_of(rows: &[Row], cell: usize) -> Option<usize> {
    rows.iter()
        .position(|row| matches!(row, Row::Sandbox(c) | Row::New(c) if *c == cell))
}

/// Scroll offset, in rows, that keeps the cursor's row on screen, together with the headings
/// right above it where they fit.
fn fit_scroll(rows: &[Row], heights: &[u16], scroll: usize, height: u16, cursor: usize) -> usize {
    let mut top = cursor;
    while top > 0 && matches!(rows[top - 1], Row::Here | Row::Other | Row::Folder(_)) {
        top -= 1;
    }
    let mut scroll = scroll.min(rows.len().saturating_sub(1)).min(top);
    while scroll < cursor && heights[scroll..=cursor].iter().sum::<u16>() > height {
        scroll += 1;
    }
    scroll
}

pub(super) fn render(frame: &mut Frame, app: &mut App, area: Rect) -> Hits {
    let mut hits = Hits::new();
    let block = panel("Sandboxes", false);
    let inner = block.inner(area);
    if inner.width == 0 || inner.height == 0 {
        frame.render_widget(block, area);
        return hits;
    }
    hits.push((inner, Hit::ScrollCards));

    let rows = app.roster();
    let cols = Columns::new(app, inner.width);
    let heights: Vec<u16> = rows.iter().map(|row| height(app, row, &cols)).collect();
    // A page is a screenful of two-line rows.
    app.grid_rows = ((inner.height / 2) as usize).max(1);
    let cursor = row_of(&rows, app.cursor).unwrap_or(0);
    app.grid_scroll = fit_scroll(&rows, &heights, app.grid_scroll, inner.height, cursor);

    // Place the rows first: the bottom border announces what does not fit, and its block is drawn
    // first. The last row is clipped, not dropped: a cut row shows that the list goes on.
    let mut placed: Vec<(Rect, usize)> = Vec::new();
    let mut y = inner.y;
    for (i, h) in heights.iter().enumerate().skip(app.grid_scroll) {
        let height = (*h).min(inner.bottom().saturating_sub(y));
        if height == 0 {
            break;
        }
        placed.push((Rect { y, height, ..inner }, i));
        y += height;
    }

    let shown = app.grid_scroll..app.grid_scroll + placed.len();
    match offscreen_hint(&rows, &shown) {
        Some(hint) => frame.render_widget(block.title_bottom(hint.right_aligned()), area),
        None => frame.render_widget(block, area),
    }

    for (band, i) in placed {
        match &rows[i] {
            Row::Here => render_here(frame, app, band),
            Row::Other => hits.extend(render_other(frame, app, band)),
            Row::Folder(head) => render_folder(frame, head, band, query(app)),
            Row::Sandbox(cell) => {
                if let Some(sb) = app.sandbox_at(*cell) {
                    let lines = sandbox_lines(app, sb, &cols, *cell == app.cursor);
                    draw_lines(frame, lines, band);
                    hits.push((band, Hit::Card(*cell)));
                }
            }
            Row::New(cell) => {
                let lines = new_line(app.here == 0, *cell == app.cursor, &cols);
                draw_lines(frame, vec![lines], band);
                hits.push((band, Hit::NewCard));
            }
        }
    }
    hits
}

/// Each line on its own line of `band`, as many as it has room for.
fn draw_lines(frame: &mut Frame, lines: Vec<Line<'static>>, band: Rect) {
    for (offset, line) in lines.into_iter().enumerate().take(band.height as usize) {
        let at = Rect {
            y: band.y + offset as u16,
            height: 1,
            ..band
        };
        frame.render_widget(Paragraph::new(line), at);
    }
}

/// What the list is not showing, written on the panel's bottom border. The second section is
/// named, not counted, when it starts below the fold.
fn offscreen_hint(rows: &[Row], shown: &Range<usize>) -> Option<Line<'static>> {
    // Counting sandboxes, so the "+ New sandbox" row and the headings do not inflate the count.
    let sandboxes = |slice: &[Row]| {
        slice
            .iter()
            .filter(|row| matches!(row, Row::Sandbox(_)))
            .count()
    };
    let above = sandboxes(&rows[..shown.start]);
    let below = sandboxes(&rows[shown.end..]);
    let other_below = rows[shown.end..].contains(&Row::Other);
    if above == 0 && below == 0 && !other_below {
        return None;
    }

    let mut spans = Vec::new();
    if above > 0 {
        spans.push(Span::styled(format!(" ↑ {above} "), theme::muted()));
    }
    if below > 0 {
        spans.push(Span::styled(format!(" ↓ {below} more"), theme::muted()));
    }
    if other_below {
        spans.push(Span::styled(
            format!(
                "{}{} ",
                if below > 0 { " · " } else { " ↓ " },
                roster::OTHER_HEADING
            ),
            theme::key(),
        ));
    } else if below > 0 {
        spans.push(Span::raw(" "));
    }
    Some(Line::from(spans))
}

/// A folder as a heading writes it: muted up to its last part, the last part in bold — the
/// folder the sandboxes are named after.
fn folder_spans(path: &std::path::Path) -> Vec<Span<'static>> {
    let (above, last) = roster::folder_parts(path);
    vec![
        Span::styled(above, theme::muted()),
        Span::styled(last, Style::default().add_modifier(Modifier::BOLD)),
    ]
}

/// A heading's line, closed by a rule across the rest of the width and the `right` spans.
fn ruled(mut left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left
        .iter()
        .chain(&right)
        .map(|s| s.content.chars().count())
        .sum();
    let fill = (width as usize).saturating_sub(used + 2);
    left.push(Span::styled(
        format!(" {} ", "─".repeat(fill)),
        theme::border(),
    ));
    left.extend(right);
    Line::from(left)
}

fn render_here(frame: &mut Frame, app: &App, band: Rect) {
    let mut left = vec![
        Span::styled(roster::HERE_HEADING, theme::title()),
        Span::raw("  "),
    ];
    left.extend(folder_spans(&app.cwd));
    left.push(Span::styled(
        match app.here {
            0 => "   no sandboxes yet".to_string(),
            n => format!("   {}", roster::count(n)),
        },
        theme::muted(),
    ));
    frame.render_widget(Paragraph::new(ruled(left, Vec::new(), band.width)), band);
}

/// The second section's heading — the button that folds it, as well as its key.
fn render_other(frame: &mut Frame, app: &App, band: Rect) -> Hits {
    // The band owns a blank line above the heading; the text goes on its last line.
    let line = Rect {
        y: band.bottom().saturating_sub(1),
        height: 1,
        ..band
    };
    let left = vec![
        Span::styled(if app.show_other { "▾ " } else { "▸ " }, theme::title()),
        Span::styled(roster::OTHER_HEADING, theme::title()),
        Span::styled(
            format!(
                "   {}",
                roster::other_count(app.elsewhere, app.other_folders())
            ),
            theme::muted(),
        ),
    ];
    let right = vec![
        Span::styled("a", theme::key()),
        Span::styled(
            if app.show_other { " hide" } else { " show" },
            theme::muted(),
        ),
    ];
    frame.render_widget(Paragraph::new(ruled(left, right, line.width)), line);
    // The text line only: a click on the gap above it does nothing.
    vec![(line, Hit::key(KeyCode::Char('a')))]
}

/// A folder heading in the second section, after a blank line that sets its sandboxes apart from
/// the folder above: the folder in bold, then how many sandboxes mount it.
fn render_folder(frame: &mut Frame, head: &FolderHead, band: Rect, query: &str) {
    let line = Rect {
        y: band.bottom().saturating_sub(1),
        height: 1,
        ..band
    };
    let mut spans = vec![Span::raw(" ")];
    if head.mountless() {
        spans.push(Span::styled(
            roster::NO_FOLDER,
            Style::default().add_modifier(Modifier::BOLD),
        ));
    } else {
        let folder = folder_spans(&head.path).into_iter().map(|span| {
            let style = span.style.add_modifier(Modifier::BOLD);
            span.style(style)
        });
        spans.extend(marked(folder.collect(), query));
    }
    if head.missing {
        spans.push(Span::styled(
            format!("  {}", roster::FOLDER_GONE),
            theme::danger(),
        ));
    }
    spans.push(Span::styled(
        format!("   {}", roster::count(head.sandboxes)),
        theme::muted(),
    ));
    frame.render_widget(Paragraph::new(Line::from(spans)), line);
}

/// A line built left to right at fixed columns.
struct Cells {
    spans: Vec<Span<'static>>,
    x: u16,
}

impl Cells {
    fn new() -> Self {
        Cells {
            spans: Vec::new(),
            x: 0,
        }
    }

    /// Pad with spaces up to column `x`.
    fn at(&mut self, x: u16) {
        if x > self.x {
            self.spans
                .push(Span::raw(" ".repeat((x - self.x) as usize)));
            self.x = x;
        }
    }

    fn push(&mut self, text: impl Into<String>, style: Style) {
        let text = text.into();
        self.x += text.chars().count() as u16;
        self.spans.push(Span::styled(text, style));
    }

    fn extend(&mut self, spans: Vec<Span<'static>>) {
        for span in spans {
            self.push(span.content, span.style);
        }
    }

    /// The line, as the cursor's row draws it when `selected`: the accent bar in the first
    /// column and the selection band across the whole width.
    fn line(mut self, width: u16, selected: bool) -> Line<'static> {
        if !selected {
            return Line::from(self.spans);
        }
        self.at(width);
        let spans = self
            .spans
            .into_iter()
            .map(|span| {
                let style = theme::on_selection(span.style);
                span.style(style)
            })
            .collect::<Vec<_>>();
        Line::from(spans).style(theme::selected())
    }
}

/// What a `/` search has typed so far — empty without one.
fn query(app: &App) -> &str {
    app.find.as_ref().map_or("", |f| f.query.as_str())
}

/// `spans` with what the search found in them reversed, as a pager marks its matches. The spans
/// are searched as one text, so a match may run across two of them.
fn marked(spans: Vec<Span<'static>>, query: &str) -> Vec<Span<'static>> {
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let found = find::spans(&text, query);
    if found.is_empty() {
        return spans;
    }
    let mut out = Vec::new();
    let mut start = 0;
    for span in spans {
        let chars: Vec<char> = span.content.chars().collect();
        let end = start + chars.len();
        let mut at = start;
        while at < end {
            let inside = found.iter().find(|r| r.contains(&at));
            let next = match inside {
                Some(r) => r.end.min(end),
                None => (found.iter().map(|r| r.start))
                    .filter(|s| *s > at)
                    .min()
                    .map_or(end, |s| s.min(end)),
            };
            let piece: String = chars[at - start..next - start].iter().collect();
            let style = match inside {
                Some(_) => span.style.add_modifier(Modifier::REVERSED),
                None => span.style,
            };
            out.push(Span::styled(piece, style));
            at = next;
        }
        start = end;
    }
    out
}

/// The selection bar: the cursor's row in shape as well as colour, so it is still found on a
/// terminal where colour carries nothing.
fn bar(cells: &mut Cells, selected: bool) {
    match selected {
        true => cells.push("▌", theme::border_focused()),
        false => cells.push(" ", Style::default()),
    }
}

/// A sandbox row's first column: the selection [`bar`] on the cursor's row, else a thin line in
/// the agent's colour, so each sandbox reads as one line patched to its agent.
fn gutter(cells: &mut Cells, selected: bool, agent: &str) {
    match selected {
        true => bar(cells, true),
        false => cells.push(GUTTER, Style::default().fg(theme::agent_color(agent))),
    }
}

/// The [`gutter`]'s thin line.
pub(super) const GUTTER: &str = "▏";

/// A sandbox's row: one or two lines.
fn sandbox_lines(app: &App, sb: &Sandbox, cols: &Columns, selected: bool) -> Vec<Line<'static>> {
    let waiting = app.approvals_for(&sb.name);
    let mut first = Cells::new();
    gutter(&mut first, selected, &sb.agent);
    first.push(
        if sb.running { "●" } else { "○" },
        Style::default().fg(if sb.running {
            theme::RUNNING
        } else {
            theme::STOPPED
        }),
    );
    first.at(cols.name.0);
    let label = clip(&sb.label, cols.name.1 as usize);
    let label = Span::styled(label, Style::default().add_modifier(Modifier::BOLD));
    first.extend(marked(vec![label], query(app)));
    let agent = Span::styled(
        clip(
            &sb.type_label(&app.catalog),
            match cols.narrow {
                true => cols.name.1 as usize,
                false => cols.runs.1 as usize,
            },
        ),
        Style::default().fg(theme::agent_color(&sb.agent)),
    );
    if !cols.narrow {
        first.at(cols.runs.0);
        first.extend(marked(vec![agent.clone()], query(app)));
    }
    first.at(cols.state.0);
    let (status, style) = status(sb, cols.state.1 as usize);
    first.push(status, style);
    let mut lines = vec![first.line(cols.width, selected)];

    if cols.lines(sb, waiting) < 2 {
        return lines;
    }
    let mut second = Cells::new();
    gutter(&mut second, selected, &sb.agent);
    second.at(cols.name.0);
    if cols.narrow {
        // The kits after the agent, as many as fit: the narrow row drops the column, not them.
        let room = (cols.name.1 as usize).saturating_sub(agent.content.chars().count() + 1);
        second.extend(marked(vec![agent], query(app)));
        let kits = kit_spans(sb, room);
        if !kits.is_empty() {
            second.push(" ", Style::default());
        }
        second.extend(marked(kits, query(app)));
    } else {
        if let Some(inside) = sb.start_inside() {
            // Warning colour: the folder is gone, so the next start opens the workspace root.
            let style = match sb.start_dir_missing() {
                true => theme::warn(),
                false => theme::muted(),
            };
            second.push(clip(&inside, cols.name.1 as usize), style);
        }
        second.at(cols.runs.0);
        second.extend(marked(kit_spans(sb, cols.runs.1 as usize), query(app)));
    }
    second.at(cols.state.0);
    for span in attention(sb, waiting, cols.state.1 as usize) {
        second.push(span.content, span.style);
    }
    lines.push(second.line(cols.width, selected));
    lines
}

/// `running · 1 session · clone`, dropped from the right until it fits: the status word stays.
fn status(sb: &Sandbox, width: usize) -> (String, Style) {
    let mut parts = sb.status_parts();
    while parts.len() > 1 && parts.join(" · ").chars().count() > width {
        parts.pop();
    }
    let style = match sb.running {
        true => Style::default().fg(theme::RUNNING),
        false => theme::muted(),
    };
    (clip(&parts.join(" · "), width), style)
}

/// The kits in their colours, as many as fit, then `+N` for the rest — by id, one word each:
/// the labels the window draws as badges have spaces, and `Host Gradle Host Maven` read as one.
fn kit_spans(sb: &Sandbox, width: usize) -> Vec<Span<'static>> {
    let badges = sb.kit_ids();
    let widths: Vec<f32> = badges
        .iter()
        .map(|(_, label)| label.chars().count() as f32)
        .collect();
    let more = |n: usize| format!("+{n}").chars().count() as f32;
    let Some(shown) = roster::fit_badges(&widths, width as f32, 1.0, more) else {
        return Vec::new();
    };
    let mut spans = Vec::new();
    for (kit, label) in &badges[..shown] {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            label.clone(),
            Style::default().fg(theme::kit_color(kit)),
        ));
    }
    if shown < badges.len() {
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(
            format!("+{}", badges.len() - shown),
            theme::muted(),
        ));
    }
    spans
}

/// A marker's glyph and colour.
fn marker(marker: CardMarker) -> (&'static str, Style) {
    match marker {
        CardMarker::Approvals => ("⚑", theme::warn()),
        CardMarker::KitsChanged => ("⟳", theme::warn()),
    }
}

/// The note in each of its wordings, longest first.
fn wordings(note: CardNote) -> Vec<String> {
    match note {
        CardNote::Unresponsive => vec![
            "⚠ not responding — press s".into(),
            "⚠ not responding".into(),
        ],
        CardNote::NoToken => vec!["⚠ no token — press A".into(), "⚠ no token".into()],
        CardNote::Approvals(waiting) => vec![
            format!("⚑ {waiting} to approve — press y"),
            format!("⚑ {waiting} to approve"),
            "⚑ approve".into(),
        ],
        CardNote::KitsChanged(kind) => {
            vec![
                format!("⟳ {} — press R", kind.note()),
                format!("⟳ {}", kind.note()),
            ]
        }
    }
}

/// Columns the attention cell needs at its longest: the note's longest wording and the markers
/// after it.
fn attention_width(sb: &Sandbox, waiting: usize) -> usize {
    let note = sb
        .card_note(waiting)
        .map_or(0, |note| wordings(note)[0].chars().count());
    let markers = sb.row_markers(waiting).len() * 2;
    note + markers
}

/// What the sandbox needs from you: the note in the longest wording that fits, then a marker for
/// every other condition (`⚑ 1 to approve — press y ⟳`).
fn attention(sb: &Sandbox, waiting: usize, width: usize) -> Vec<Span<'static>> {
    let markers = sb.row_markers(waiting);
    let room = width.saturating_sub(markers.len() * 2);
    let mut spans = Vec::new();
    if let Some(note) = sb.card_note(waiting) {
        // A sandbox that cannot run at all is a danger, not a warning.
        let style = match note.is_danger() {
            true => theme::danger(),
            false => theme::warn().add_modifier(Modifier::BOLD),
        };
        if let Some(text) = wordings(note)
            .into_iter()
            .find(|text| text.chars().count() <= room)
        {
            spans.push(Span::styled(text, style));
        }
    }
    for m in markers {
        let (glyph, style) = marker(m);
        if !spans.is_empty() {
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled(glyph, style));
    }
    spans
}

/// The "+ New sandbox" row that closes the current folder's section. It lights up only when
/// focused, like every other row.
fn new_line(folder_empty: bool, selected: bool, cols: &Columns) -> Line<'static> {
    let mut cells = Cells::new();
    bar(&mut cells, selected);
    cells.push(
        "+",
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    );
    cells.at(cols.name.0);
    cells.push("New sandbox", Style::default().add_modifier(Modifier::BOLD));
    match cols.narrow {
        true => cells.push("  ", Style::default()),
        // In the middle column where the title leaves room, else one gap after the title.
        false => cells.at(cols.runs.0.max(cells.x + GAP)),
    }
    cells.push(
        if folder_empty {
            "none here yet — press n"
        } else if selected {
            "press ↵"
        } else {
            "press n"
        },
        theme::muted(),
    );
    cells.line(cols.width, selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kits::Catalog;
    use crate::model;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The whole screen at `w`×`h`, one string per line.
    fn screen(app: &mut App, w: u16, h: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, app))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect()
            })
            .collect()
    }

    /// Two sandboxes here, three in two other folders.
    fn mixed() -> Vec<Sandbox> {
        let mut out = vec![
            model::sample("here-1", Some("claude-anthropic"), &[]),
            model::sample("here-2", Some("claude-anthropic"), &[]),
        ];
        for (i, dir) in ["/work/api", "/work/api", "/work/ui"].iter().enumerate() {
            let mut sb = model::sample(&format!("far-{i}"), Some("claude-anthropic"), &[]);
            sb.workspace = std::path::PathBuf::from(dir);
            out.push(sb);
        }
        out
    }

    /// A sandbox with something in every cell of both lines.
    fn full() -> Sandbox {
        let mut sb = model::sample(
            "claude-litellm-proj",
            Some("claude-litellm"),
            &["jdk25", "rtk", "web"],
        );
        sb.label = "Gateway Claude".into();
        sb.running = true;
        sb.sessions = 2;
        sb.start_dir = Some("api".into());
        sb.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        sb
    }

    #[test]
    fn the_new_rows_hint_never_runs_into_its_label() {
        let short = |label: &str| {
            let mut sb = model::sample("short", Some("claude-anthropic"), &[]);
            sb.label = label.into();
            sb
        };
        for (sandboxes, hint) in [
            (Vec::new(), "none here yet — press n"),
            (vec![short("api")], "press n"),
            (vec![short("a label wider than New sandbox")], "press n"),
        ] {
            let app = App::for_test(Catalog::load().unwrap(), sandboxes);
            let cols = Columns::new(&app, 100);
            let line = text(&new_line(app.here == 0, false, &cols));
            let after = line
                .split_once("New sandbox")
                .unwrap_or_else(|| panic!("{line:?}"))
                .1;
            assert!(after.starts_with("  "), "{line:?}");
            assert_eq!(after.trim_start(), hint, "{line:?}");
            let at = line[..line.find(hint).unwrap()].chars().count() as u16;
            assert!(at >= cols.runs.0, "{line:?} {cols:?}");
        }
    }

    #[test]
    fn each_row_is_patched_to_its_agent_by_the_gutter() {
        let app = App::for_test(Catalog::load().unwrap(), vec![full()]);
        let sb = &app.sandboxes[0];
        let cols = Columns::new(&app, 116);
        for line in sandbox_lines(&app, sb, &cols, false) {
            let first = &line.spans[0];
            assert_eq!(first.content, GUTTER, "{line:?}");
            assert_eq!(first.style.fg, Some(theme::agent_color(&sb.agent)));
        }
        for line in sandbox_lines(&app, sb, &cols, true) {
            assert_eq!(line.spans[0].content, "▌", "the cursor's bar: {line:?}");
        }
        let new = text(&new_line(false, false, &cols));
        assert!(new.starts_with(' '), "the New row is no agent's: {new:?}");
    }

    #[test]
    fn a_row_shows_every_fact_in_its_column() {
        let mut app = App::for_test(Catalog::load().unwrap(), vec![full()]);
        app.select(0);
        let cols = Columns::new(&app, 116);
        let lines = sandbox_lines(&app, &app.sandboxes[0], &cols, false);
        let (first, second) = (text(&lines[0]), text(&lines[1]));
        let at = |line: &str, x: u16| line.chars().skip(x as usize).collect::<String>();
        assert!(first.starts_with("▏● Gateway Claude"), "{first:?}");
        assert!(
            at(&first, cols.runs.0).starts_with("Claude Code · LiteLLM acme"),
            "{first:?}"
        );
        assert!(
            at(&first, cols.state.0).starts_with("running · 2 sessions"),
            "{first:?}"
        );
        assert!(at(&second, cols.name.0).starts_with("› api"), "{second:?}");
        assert!(
            at(&second, cols.runs.0).starts_with("jdk25 rtk web"),
            "{second:?}"
        );
        assert!(
            at(&second, cols.state.0).starts_with("⟳ update ready — press R"),
            "{second:?}"
        );
    }

    #[test]
    fn the_label_gives_way_before_the_agent() {
        let mut long = full();
        long.label = "A label much longer than the room an eighty-column terminal has".into();
        let app = App::for_test(Catalog::load().unwrap(), vec![long]);
        let cols = Columns::new(&app, 76);
        assert!(!cols.narrow);
        let first = text(&sandbox_lines(&app, &app.sandboxes[0], &cols, false)[0]);
        assert!(first.contains("Claude Code · LiteLLM acme"), "{first:?}");
        assert!(
            first.contains("A label much longer") && first.contains('…'),
            "the label is cut: {first:?}"
        );
    }

    #[test]
    fn the_columns_line_up_and_share_the_width() {
        let app = App::for_test(Catalog::load().unwrap(), vec![full()]);
        let cols = Columns::new(&app, 116);
        assert!(!cols.narrow);
        assert_eq!(cols.name, (LEAD, "Gateway Claude".chars().count() as u16));
        assert_eq!(cols.runs.0, cols.name.0 + cols.name.1 + GAP);
        assert_eq!(cols.runs.0 + cols.runs.1 + GAP, cols.state.0);
        assert_eq!(cols.state.0 + cols.state.1, 116);

        // Narrow: two columns, the agent under the name with what fits of the kits after it,
        // and no start folder.
        let narrow = Columns::new(&app, 64);
        assert!(narrow.narrow);
        let lines = sandbox_lines(&app, &app.sandboxes[0], &narrow, false);
        let second = text(&lines[1]);
        assert!(
            second.contains("Claude Code · LiteLLM acme jdk25"),
            "{second:?}"
        );
        assert!(!second.contains("› api"), "{second:?}");
        assert!(lines.iter().all(|l| text(l).chars().count() <= 64));
    }

    #[test]
    fn a_row_with_nothing_more_to_say_is_one_line() {
        let app = App::for_test(
            Catalog::load().unwrap(),
            vec![model::sample("a", Some("claude-anthropic"), &[])],
        );
        let cols = Columns::new(&app, 116);
        assert_eq!(
            sandbox_lines(&app, &app.sandboxes[0], &cols, false).len(),
            1
        );
        assert_eq!(height(&app, &Row::Sandbox(0), &cols), 1);
    }

    #[test]
    fn kits_that_do_not_fit_are_counted() {
        let sb = model::sample(
            "a",
            Some("claude-litellm"),
            &["jdk25", "rtk", "statusline", "ponytail", "headroom", "web"],
        );
        let spans = kit_spans(&sb, 24);
        let line = spans.iter().map(|s| s.content.as_ref()).collect::<String>();
        assert_eq!(line, "jdk25 rtk statusline +3");
        assert!(line.chars().count() <= 24);
        let all = kit_spans(&sb, 80);
        assert!(!all.iter().any(|s| s.content.starts_with('+')));
    }

    #[test]
    fn the_attention_cell_names_the_note_and_marks_the_rest() {
        let mut sb = model::sample("a", Some("claude-anthropic"), &[]);
        sb.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        let line = |sb: &Sandbox, waiting, width| {
            attention(sb, waiting, width)
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        };
        assert_eq!(line(&sb, 0, 30), "⟳ update ready — press R");
        assert_eq!(line(&sb, 0, 16), "⟳ update ready");
        assert_eq!(line(&sb, 1, 30), "⚑ 1 to approve — press y ⟳");
        assert_eq!(line(&sb, 1, 18), "⚑ 1 to approve ⟳");
        sb.unresponsive = true;
        assert_eq!(line(&sb, 1, 40), "⚠ not responding — press s ⚑ ⟳");
    }

    #[test]
    fn the_selected_row_has_a_bar_and_a_band() {
        let mut app = App::for_test(Catalog::load().unwrap(), mixed());
        app.select(1);
        let shown = screen(&mut app, 110, 30);
        let on_bar = |l: &String| l.chars().nth(2) == Some('▌');
        let bars: Vec<&String> = shown.iter().filter(|l| on_bar(l)).collect();
        assert_eq!(bars.len(), 1, "one row is the selection: {shown:#?}");
        assert!(bars[0].contains("here-2"), "{bars:?}");

        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal
            .draw(|frame| super::super::draw(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let y = shown.iter().position(on_bar).unwrap() as u16;
        let band = (2..108u16)
            .filter(|x| buffer[(*x, y)].style().bg == theme::selected().bg)
            .count();
        assert!(band > 90, "the band crosses the row: {band}");
        let other = shown.iter().position(|l| l.contains("here-1")).unwrap() as u16;
        assert!((2..108u16).all(|x| buffer[(x, other)].style().bg != theme::selected().bg));
    }

    #[test]
    fn the_new_row_lights_up_only_under_the_cursor() {
        let mut app = App::for_test(Catalog::load().unwrap(), mixed());
        let new = |shown: &[String]| {
            shown
                .iter()
                .find(|l| l.contains("New sandbox"))
                .cloned()
                .unwrap()
        };
        app.select(0);
        let away = new(&screen(&mut app, 110, 30));
        assert!(!away.contains('▌') && away.contains("press n"), "{away}");
        app.select(app.here);
        let on = new(&screen(&mut app, 110, 30));
        assert!(on.contains('▌') && on.contains("press ↵"), "{on}");
    }

    #[test]
    fn other_folders_sit_under_their_folder_headings() {
        let mut app = App::for_test(Catalog::load().unwrap(), mixed());
        let shown = screen(&mut app, 110, 40);
        let find = |needle: &str| {
            shown
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} in {shown:#?}"))
        };
        let heading = find(roster::OTHER_HEADING);
        assert!(shown[heading].contains("3 sandboxes in 2 folders"));
        assert!(heading < find("/work/api") && find("/work/api") < find("far-0"));
        assert!(find("far-1") < find("/work/ui") && find("/work/ui") < find("far-2"));
        assert_eq!(
            shown.iter().filter(|l| l.contains("/work/api")).count(),
            1,
            "the folder is said once"
        );
        for (folder, count) in [("/work/api", "2 sandboxes"), ("/work/ui", "1 sandbox")] {
            let at = find(folder);
            assert!(shown[at].trim_end().ends_with(count), "{:?}", shown[at]);
            assert!(shown[at - 1].trim().is_empty(), "{:?}", shown[at - 1]);
        }
    }

    #[test]
    fn scrolling_brings_the_cursor_and_its_folder_into_view() {
        let app = App::for_test(Catalog::load().unwrap(), mixed());
        let rows = app.roster();
        let heights = vec![1; rows.len()];
        let last = row_of(&rows, app.slots() - 1).unwrap();
        assert!(matches!(rows[last - 1], Row::Folder(_)));
        // Down from the top: the view ends on the cursor, its folder heading in view.
        let scroll = fit_scroll(&rows, &heights, 0, 4, last);
        assert_eq!(scroll, last - 3);
        // Up into the first other folder: its heading and the section's come along.
        let first = row_of(&rows, app.here + 1).unwrap();
        assert_eq!(
            rows[first - 2..first],
            [Row::Other, rows[first - 1].clone()]
        );
        assert!(matches!(rows[first - 1], Row::Folder(_)));
        assert_eq!(fit_scroll(&rows, &heights, last, 4, first), first - 2);
        // Back up to the top: the first heading is in view again.
        assert_eq!(fit_scroll(&rows, &heights, scroll, 4, 1), 0);
        // No room for a heading: the cursor's row alone.
        assert_eq!(fit_scroll(&rows, &heights, 0, 1, last), last);
    }
}

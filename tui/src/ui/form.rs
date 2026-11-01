//! The full-screen forms: create a sandbox, edit an existing sandbox's kits, change which
//! Anthropic account it authenticates with, manage the host's stored secrets and its shared
//! agent-skills store, and connect an editor or app to a sandbox over SSH.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::{panel, Hits};
use crate::app::{
    AccountChoice, App, ApprovalRow, AuthRow, ConnMode, ConnRow, ConnSwitchRow, ConnectRow,
    CreateForm, CreateRow, Empty, FoldersRow, Hit, KitsRow, Screen, SecretRow, SkillRow,
    StartDirRow, TargetLaunch, TextInput,
};
use crate::create::account_notes;
use crate::model::{self, Auth};
use crate::models::{Model, Overrides, Slot};
use crate::repo_skills_ui::{RepoScope, RepoSkillRow};
use crate::sbx::SkillsMode;
use crate::theme;

mod wizard;

/// A form line plus which focusable row (if any) it belongs to.
struct Lines<'a> {
    lines: Vec<Line<'a>>,
    focus_line: Option<usize>,
    /// Inner width, so the focused row's highlight can span the full panel.
    width: u16,
    /// Where each focusable row landed, as (line index, form row index) — the raw material for
    /// the click boxes, resolved to screen coordinates once the scroll offset is known.
    rows: Vec<(usize, usize)>,
    /// What the row under the cursor means, for the help strip ([`Lines::explain`]).
    help: Option<Strip>,
    /// The most lines any row's help on this screen takes: the strip's height ([`framed`]).
    help_need: usize,
}

/// The help strip's text: the name of the row under the cursor, for the rule over the strip,
/// and what it means, wrapped to the panel.
struct Strip {
    title: String,
    lines: Vec<Line<'static>>,
}

impl<'a> Lines<'a> {
    fn new(area: Rect) -> Self {
        Self {
            lines: Vec::new(),
            focus_line: None,
            // minus the two borders and the block's one column of padding on each side
            width: area.width.saturating_sub(4),
            rows: Vec::new(),
            help: None,
            help_need: 0,
        }
    }

    /// What a row means, for the help strip of [`framed`]: kept while the cursor is on the row, and
    /// measured either way, since the strip is as tall as the longest help on the screen.
    fn explain(&mut self, focused: bool, title: impl Into<String>, text: &[(String, Style)]) {
        let width = self.width as usize;
        let lines: Vec<Line<'static>> = text
            .iter()
            .flat_map(|(piece, style)| {
                wrap(piece, width)
                    .into_iter()
                    .map(|line| Line::from(Span::styled(line, *style)))
            })
            .collect();
        self.help_need = self.help_need.max(lines.len());
        if focused {
            self.help = Some(Strip {
                title: title.into(),
                lines,
            });
        }
    }

    /// [`Lines::explain`] for plain lines of core's text.
    fn explain_lines(&mut self, focused: bool, title: impl Into<String>, lines: &[String]) {
        let text: Vec<(String, Style)> = lines
            .iter()
            .map(|line| (line.clone(), Style::default()))
            .collect();
        self.explain(focused, title, &text);
    }

    fn blank(&mut self) {
        self.lines.push(Line::from(""));
    }

    fn section(&mut self, title: &'a str) {
        self.lines.push(Line::from(Span::styled(
            title,
            theme::muted().add_modifier(Modifier::BOLD),
        )));
    }

    /// A section heading with its gloss on the same line.
    /// Where the gloss does not fit, the heading stands alone: a heading never wraps.
    fn section_with(&mut self, title: &'a str, gloss: &'a str) {
        if title.chars().count() + 3 + gloss.chars().count() > self.width as usize {
            return self.section(title);
        }
        self.lines.push(Line::from(vec![
            Span::styled(title, theme::muted().add_modifier(Modifier::BOLD)),
            Span::styled(format!("   {gloss}"), theme::muted()),
        ]));
    }

    /// Push a row that can hold focus; highlights it and remembers where it landed.
    fn row(&mut self, spans: Vec<Span<'a>>, focused: bool, index: usize) {
        let mut spans = fit_row(spans, self.width as usize);
        self.rows.push((self.lines.len(), index));
        if !focused {
            self.lines.push(Line::from(spans));
            return;
        }
        self.focus_line = Some(self.lines.len());
        mark_cursor(&mut spans);
        // Muted grey vanishes on the band: every span takes the selection colour, as on the list.
        for span in &mut spans {
            span.style = theme::on_selection(span.style);
        }
        // Pad to the full width so the highlight reads as a selection bar, not a ragged patch.
        let used = Line::from(spans.clone()).width() as u16;
        if used < self.width {
            spans.push(Span::raw(" ".repeat((self.width - used) as usize)));
        }
        self.lines.push(Line::from(spans).style(theme::selected()));
    }

    /// One literal line — a command to type, or a row of columns that must stay aligned.
    fn note(&mut self, text: impl Into<String>) {
        self.lines
            .push(Line::from(Span::styled(text.into(), theme::muted())));
    }

    /// Prose under the row it belongs to, wrapped to the panel.
    fn para(&mut self, text: impl AsRef<str>) {
        self.wrapped(text.as_ref(), theme::muted());
    }

    /// A note about something that will bite: drawn in the warning colour, not the muted one.
    fn warn(&mut self, text: impl AsRef<str>) {
        self.wrapped(text.as_ref(), theme::warn());
    }

    fn wrapped(&mut self, text: &str, style: Style) {
        for line in wrap(text, self.width as usize) {
            self.lines.push(Line::from(Span::styled(line, style)));
        }
    }

    /// The focused row and the last line that belongs with it — its explanation, and whatever
    /// heading opens the next block.
    fn focus_block(&self) -> Option<(usize, usize)> {
        let start = self.focus_line?;
        let end = self
            .rows
            .iter()
            .map(|(line, _)| *line)
            .find(|line| *line > start)
            .map(|next| next - 1)
            .unwrap_or_else(|| self.lines.len().saturating_sub(1));
        Some((start, end.max(start)))
    }

    /// Click boxes for every focusable row currently on screen.
    fn hits(&self, area: Rect, scroll: u16) -> Hits {
        self.rows
            .iter()
            .filter_map(|(line, index)| {
                let y = (*line as u16).checked_sub(scroll)? + area.y + 1;
                (y < area.bottom().saturating_sub(1)).then_some((
                    Rect {
                        x: area.x + 1,
                        y,
                        width: area.width.saturating_sub(2),
                        height: 1,
                    },
                    Hit::FormRow(*index),
                ))
            })
            .collect()
    }
}

/// A row cut to `width` columns, with an ellipsis at the cut, since rows cannot wrap. A text
/// field's caret stays in view: the text before it gives way at its start instead.
fn fit_row(mut spans: Vec<Span<'_>>, width: usize) -> Vec<Span<'_>> {
    let total = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    if total(&spans) <= width || width < 4 {
        return spans;
    }
    let is_caret = |s: &Span| s.style.add_modifier.contains(Modifier::REVERSED) && s.width() == 1;
    if let Some(caret) = spans.iter().position(is_caret).filter(|c| *c > 0) {
        // Up to and including the caret, with a column left for the `…` that ends a cut row.
        let past = total(&spans[..=caret]).saturating_sub(width - 1);
        let before = &spans[caret - 1];
        if past > 0 && past < before.width() {
            let mut dropped = 0;
            let kept: String = before
                .content
                .chars()
                .skip_while(|c| {
                    // One more column than `past`, for the `…` that stands in for the start.
                    let skip = dropped <= past;
                    if skip {
                        dropped += theme::unicode_width(*c);
                    }
                    skip
                })
                .collect();
            spans[caret - 1] = Span::styled(format!("…{kept}"), before.style);
        }
    }
    if total(&spans) <= width {
        return spans;
    }
    let mut out = Vec::new();
    let mut used = 0;
    for span in spans {
        if used + span.width() < width {
            used += span.width();
            out.push(span);
            continue;
        }
        let mut text = String::new();
        for c in span.content.chars() {
            if used + theme::unicode_width(c) >= width {
                break;
            }
            used += theme::unicode_width(c);
            text.push(c);
        }
        let style = span.style.remove_modifier(Modifier::REVERSED);
        out.push(Span::styled(text, span.style));
        out.push(Span::styled("…", style));
        break;
    }
    out
}

/// The cursor's mark: a glyph in the row's first column, which every row leaves blank for it, as
/// well as the band — a band alone vanishes on a terminal without colour.
pub(super) const CURSOR: &str = "›";

/// Put [`CURSOR`] in the first column of a focused row, where that column is blank.
fn mark_cursor(spans: &mut Vec<Span<'_>>) {
    let Some(first) = spans.first_mut() else {
        return;
    };
    let Some(rest) = first.content.strip_prefix(' ') else {
        return;
    };
    let rest = Span::styled(rest.to_string(), first.style);
    *first = Span::styled(CURSOR, theme::key());
    spans.insert(1, rest);
}

/// Wrap one already-indented sentence to `width` columns, carrying its indent onto every line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let indent = text.chars().take_while(|c| *c == ' ').count();
    // Too narrow for the indent and a word: leave it to clipping, not one character per line.
    if text.chars().count() <= width || width <= indent + 8 {
        return vec![text.to_string()];
    }
    let pad = " ".repeat(indent);
    super::wrap_text(text.trim_start(), width - indent)
        .into_iter()
        .map(|line| format!("{pad}{line}"))
        .collect()
}

/// Scroll so the focused row — and the note under it — stay visible in `height` rows of viewport.
fn scroll_for(block: Option<(usize, usize)>, total: usize, height: u16) -> u16 {
    let height = height.saturating_sub(2).max(1) as usize; // minus the panel border
    let (start, end) = block.unwrap_or((0, 0));
    // The block's end comes into view, but the row itself never leaves the top.
    let want = end.min(start + height - 1);
    if total <= height || want < height {
        return 0;
    }
    (want + 1 - height) as u16
}

/// Text field content with a reversed cell standing in for the caret.
fn input_spans(input: &TextInput, focused: bool) -> Vec<Span<'static>> {
    field_spans(input, focused, false, "(auto)")
}

/// A field whose value is a secret: only ever drawn as dots, whether it has focus or not.
fn secret_spans(input: &TextInput, focused: bool) -> Vec<Span<'static>> {
    field_spans(input, focused, true, "(paste it here)")
}

fn field_spans(
    input: &TextInput,
    focused: bool,
    mask: bool,
    empty_hint: &'static str,
) -> Vec<Span<'static>> {
    let value = if mask {
        "•".repeat(input.value.chars().count())
    } else {
        input.value.clone()
    };
    if !focused {
        let span = if input.value.is_empty() {
            Span::styled(empty_hint, theme::muted())
        } else {
            Span::raw(value)
        };
        return vec![span];
    }
    let chars: Vec<char> = value.chars().collect();
    let before: String = chars[..input.cursor.min(chars.len())].iter().collect();
    let at = chars.get(input.cursor).copied().unwrap_or(' ');
    let after: String = chars
        .get(input.cursor + 1..)
        .map(|c| c.iter().collect())
        .unwrap_or_default();
    vec![
        Span::raw(before),
        Span::styled(
            at.to_string(),
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(after),
    ]
}

/// How a button on a form's last line looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    /// The form's main action, first on the line: in brackets, so it stands out in a terminal
    /// without colour too.
    Primary,
    Plain,
    /// An action that costs something (a rebuild): in the danger colour, primary or not.
    Danger,
    /// In its place but doing nothing, like Back on the wizard's first step.
    Inert,
}

fn button(label: &str, focused: bool, look: Look) -> Vec<Span<'static>> {
    let base = match look {
        Look::Danger => theme::danger(),
        Look::Primary => Style::default().fg(theme::ACCENT).bold(),
        Look::Plain => Style::default().fg(theme::ACCENT),
        Look::Inert => theme::muted(),
    };
    let style = match (focused, look) {
        (true, Look::Inert) => base.add_modifier(Modifier::REVERSED),
        (true, _) => base.add_modifier(Modifier::REVERSED | Modifier::BOLD),
        (false, _) => base,
    };
    let text = match look {
        Look::Primary => format!("[ {label} ]"),
        Look::Plain | Look::Danger | Look::Inert => format!("  {label}  "),
    };
    vec![Span::styled(text, style)]
}

/// The create form, which is drawn as the step-by-step wizard (`wizard`).
pub(super) fn render_create(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    wizard::render(frame, app, area)
}

/// How to fix a credential the chosen backend needs and this host lacks.
fn missing_secret_notes(out: &mut Lines<'_>, missing: &[String]) {
    out.warn(format!(
        "     ⚠ {} is not stored on this host, so this sandbox would have no access.",
        missing.join(" and ")
    ));
    out.warn("       Store it first — a running sandbox picks it up later too:");
    for service in missing {
        out.note(format!(
            "         {}",
            crate::kits::secret_set_hint(service)
        ));
    }
}

/// The setup-token pick rows under the token radio: the stored tokens, "new token…", and a new
/// token's name and value.
fn token_rows(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: Option<CreateRow>,
    row_index: &dyn Fn(CreateRow) -> usize,
) {
    for (i, token) in form.connection.tokens.iter().enumerate() {
        out.row(
            token_spans(
                form.connection.account == AccountChoice::Named(token.clone()),
                token,
                crate::create::token_users(app.token_users(token)),
            ),
            focused == Some(CreateRow::TokenPick(i)),
            row_index(CreateRow::TokenPick(i)),
        );
    }
    out.row(
        token_spans(
            form.connection.account == AccountChoice::New,
            "new token…",
            "paste a new token and name it".into(),
        ),
        focused == Some(CreateRow::TokenNew),
        row_index(CreateRow::TokenNew),
    );
    if form.connection.account == AccountChoice::New {
        let mut name_spans = vec![Span::styled("        name  ", theme::muted())];
        name_spans.extend(input_spans(
            &form.connection.token_name,
            focused == Some(CreateRow::TokenName),
        ));
        out.row(
            name_spans,
            focused == Some(CreateRow::TokenName),
            row_index(CreateRow::TokenName),
        );
        let mut token_spans = vec![Span::styled("       token  ", theme::muted())];
        token_spans.extend(secret_spans(
            &form.connection.token,
            focused == Some(CreateRow::Token),
        ));
        out.row(
            token_spans,
            focused == Some(CreateRow::Token),
            row_index(CreateRow::Token),
        );
    }
}

/// The Connection step's row on a LiteLLM provider: which connection the sandbox is built on,
/// picked with left and right, its host shown before anything is saved.
fn litellm_row(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: Option<CreateRow>,
    row_index: &dyn Fn(CreateRow) -> usize,
) {
    let row = CreateRow::LitellmConn;
    let mut spans = vec![Span::styled(
        format!("{:>14}  ", "connection"),
        theme::muted(),
    )];
    let picked = form
        .connection
        .litellm
        .as_deref()
        .and_then(|name| form.connection.connections.iter().find(|c| c.name == name));
    match picked {
        Some(conn) => {
            spans.push(Span::styled(
                format!("‹ {} · {} ›", conn.name, crate::litellm::host(&conn.url)),
                Style::default().fg(theme::ACCENT),
            ));
            if form.connection.last_litellm.as_deref() == Some(conn.name.as_str()) {
                spans.push(Span::styled(" (last used)", theme::muted()));
            }
            if app.litellm_key_missing(&conn.name) {
                spans.push(Span::styled(" key missing", theme::warn()));
            }
        }
        None => spans.push(Span::styled(
            "‹ new connection… ›",
            Style::default().fg(theme::ACCENT),
        )),
    }
    out.row(spans, focused == Some(row), row_index(row));
    if let Some(conn) = picked.filter(|c| app.litellm_key_missing(&c.name)) {
        out.warn(format!(
            "                {}",
            crate::create::missing_key_note(conn)
        ));
    }
    if picked.is_none() {
        new_connection_rows(out, form, focused, row_index);
    }
}

/// *new connection…*: made with the sandbox — its key stored first, then its file.
fn new_connection_rows(
    out: &mut Lines<'_>,
    form: &CreateForm,
    focused: Option<CreateRow>,
    row_index: &dyn Fn(CreateRow) -> usize,
) {
    let typed = &form.connection.new_conn;
    let host = crate::litellm::check_url(typed.url.value.trim())
        .ok()
        .map(|()| format!("  host {}", crate::litellm::host(typed.url.value.trim())));
    for (field, label, input, hint) in [
        (CreateRow::LitellmName, "name", &typed.name, "(a–z, 0–9, -)"),
        (CreateRow::LitellmUrl, "URL", &typed.url, "(https://…)"),
        (CreateRow::LitellmKey, "key", &typed.key, "(paste it here)"),
    ] {
        let on = focused == Some(field);
        let mut spans = vec![Span::styled(format!("{label:>14}  "), theme::muted())];
        spans.extend(field_spans(input, on, field == CreateRow::LitellmKey, hint));
        if let (CreateRow::LitellmUrl, Some(host)) = (field, &host) {
            spans.push(Span::styled(host.clone(), theme::muted()));
        }
        out.row(spans, on, row_index(field));
    }
}

/// The *Host caches* and *Additional kits* blocks, with the warnings a ticked kit brings.
fn kits_rows(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: Option<CreateRow>,
    row_index: &dyn Fn(CreateRow) -> usize,
) {
    let catalog = &app.catalog;
    let addons = form.selected_addons(catalog);
    // No blank line above either heading: at 110x40 the step needs every line.
    let order = form.addon_order(catalog);
    let host_rows = order
        .iter()
        .take_while(|i| form.kits.host.get(**i).is_some_and(Option::is_some))
        .count();
    // The `only now` markers form one column across both blocks, after the longest text.
    let text = |i: usize| match form.kits.host.get(i).and_then(Option::as_ref) {
        Some(found) => host_cache_text(&catalog.addons[i], found),
        None => catalog.addons[i].kit.display_name.clone(),
    };
    let column = order
        .iter()
        .map(|i| text(*i).chars().count())
        .max()
        .unwrap_or(0);
    for (n, &i) in order.iter().enumerate() {
        if n == 0 && host_rows > 0 {
            out.section_with(
                "Host caches",
                "read-only, from this host — greyed where none is found",
            );
        }
        if n == host_rows {
            out.section("Additional kits");
        }
        let addon = &catalog.addons[i];
        let row_focused = focused == Some(CreateRow::Addon(i));
        let marker = addon
            .create_time_only()
            .then_some(Marker::Quiet(crate::create::ONLY_NOW));
        let detail = form.kits.host.get(i).and_then(Option::as_ref);
        let width = out.width as usize;
        let checked = form.kits.addons[i];
        let spans = match detail {
            Some(found) => host_cache_spans(addon, found, checked, marker, column, width),
            None => addon_spans(
                addon.kit.name.as_str(),
                &addon.kit.display_name,
                checked,
                marker,
                None,
                column,
                width,
            ),
        };
        out.row(spans, row_focused, row_index(CreateRow::Addon(i)));
    }
    if addons.iter().any(|a| a == "web") {
        out.warn(
            "     ⚠ web lets this sandbox reach every host on the internet, not just the gateway's.",
        );
    }
    if addons.iter().any(|a| a == "headroom") {
        let agent = form
            .effective_backend(catalog)
            .map(|b| b.agent.as_str())
            .unwrap_or("");
        out.warn(format!("     {}", crate::kits::headroom_warning(agent)));
    }
}

/// Name and folder, Options, Ports, Blocked hosts and the MCP servers.
fn workspace_rows(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: Option<CreateRow>,
    row_index: &dyn Fn(CreateRow) -> usize,
) {
    let catalog = &app.catalog;
    // No blank line above this heading either: the step needs every line.
    out.section_with(
        "Name and folder",
        "your label, and where the agent opens inside the workspace",
    );
    let mut label = input_spans(&form.workspace.name, focused == Some(CreateRow::Name));
    // The first character typed replaces a generated label (core's `field_key`); the tag says so.
    if !form.workspace.name_touched {
        let tag = format!("  {}", crate::create::GENERATED_TAG);
        label.push(Span::styled(tag, theme::muted()));
    }
    out.row(
        field_row_spans("label", label),
        focused == Some(CreateRow::Name),
        row_index(CreateRow::Name),
    );

    if form.start_dir_offered(catalog) {
        out.row(
            field_row_spans(
                "opens in",
                vec![
                    Span::styled(
                        crate::startdir::describe(&app.cwd, form.workspace.start_dir.as_deref()),
                        Style::default().bold(),
                    ),
                    Span::styled(
                        format!(
                            "   {} {}",
                            theme::key_name("↵", theme::caps()),
                            match form.workspace.start_dir.is_some() {
                                true => "change",
                                false => "pick a subfolder",
                            }
                        ),
                        theme::muted(),
                    ),
                ],
            ),
            focused == Some(CreateRow::StartDir),
            row_index(CreateRow::StartDir),
        );
    }

    // No blank line above this heading either: the step needs every line.
    out.section("Options");
    out.row(
        option_spans(
            form.workspace.clone,
            "clone mode",
            crate::create::field_gloss(CreateRow::Clone),
            out.width as usize,
        ),
        focused == Some(CreateRow::Clone),
        row_index(CreateRow::Clone),
    );
    if form.git_guard_offered() {
        out.row(
            option_spans(
                form.workspace.git_guard,
                ".git read-only",
                crate::create::field_gloss(CreateRow::GitGuard),
                out.width as usize,
            ),
            focused == Some(CreateRow::GitGuard),
            row_index(CreateRow::GitGuard),
        );
    }

    if form.skills_offered(catalog) {
        out.row(
            skills_spans(form.workspace.skills, out.width as usize),
            focused == Some(CreateRow::Skills),
            row_index(CreateRow::Skills),
        );
    }

    out.section_with("Network", "optional, space- or comma-separated");
    // "(none)", not "(auto)": an empty ports field publishes nothing.
    out.row(
        field_row_spans(
            "ports",
            field_spans(
                &form.workspace.ports,
                focused == Some(CreateRow::Ports),
                false,
                "(none)",
            ),
        ),
        focused == Some(CreateRow::Ports),
        row_index(CreateRow::Ports),
    );

    out.row(
        field_row_spans(
            "blocked hosts",
            field_spans(
                &form.workspace.deny_network,
                focused == Some(CreateRow::DenyNetwork),
                false,
                "(none)",
            ),
        ),
        focused == Some(CreateRow::DenyNetwork),
        row_index(CreateRow::DenyNetwork),
    );

    if !form.workspace.mcp.is_empty() {
        out.section_with(
            "MCP servers",
            "optional — fixed at creation, credentials stay on the host",
        );
        for (i, server) in form.workspace.mcp.iter().enumerate() {
            out.row(
                addon_spans(
                    &server.name,
                    &server.transport,
                    form.workspace.static_mcp[i],
                    (!server.ready()).then_some(Marker::Warn(server.status.as_str())),
                    None,
                    0,
                    out.width as usize,
                ),
                focused == Some(CreateRow::Mcp(i)),
                row_index(CreateRow::Mcp(i)),
            );
        }
    }
}

pub(super) fn render_connect(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Connect(form) = &app.screen else {
        return Hits::new();
    };
    let Some(view) = app.connect_view() else {
        return Hits::new();
    };
    let rows = form.rows();
    let focused = form.focused();
    let row_index = |row: ConnectRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    // Core's lines come unindented; a warning's first line carries the ⚠.
    let warn_lines = |out: &mut Lines, lines: &[String]| {
        for (i, line) in lines.iter().enumerate() {
            out.warn(match i {
                0 => format!("     ⚠ {line}"),
                _ => format!("       {line}"),
            });
        }
    };
    // …and in the strip, where a warning reads as one sentence after the steps.
    let warned = |lines: &[String]| match lines.is_empty() {
        true => Vec::new(),
        false => vec![(format!("⚠ {}", lines.join(" ")), theme::warn())],
    };

    let mut out = Lines::new(area);
    out.section("SSH endpoint");
    out.row(
        vec![
            Span::raw("   "),
            Span::styled(view.host.clone(), Style::default().bold()),
            Span::styled(format!("   {}", view.state), theme::muted()),
        ],
        false,
        usize::MAX,
    );
    for line in &view.workspace {
        out.note(format!("     {line}"));
    }

    if view.ssh_missing {
        out.blank();
        warn_lines(&mut out, &view.ssh_warning);
        out.row(
            button(
                "Set up SSH now",
                focused == ConnectRow::SetupSsh,
                Look::Plain,
            ),
            focused == ConnectRow::SetupSsh,
            row_index(ConnectRow::SetupSsh),
        );
        out.explain_lines(
            focused == ConnectRow::SetupSsh,
            "Set up SSH now",
            &view.ssh_notes,
        );
    }

    out.blank();
    out.section("Connect with");
    for target in &view.targets {
        let missing = matches!(target.launch, TargetLaunch::NotOnPath(_));
        let spans = vec![
            Span::raw("   "),
            Span::raw(target.title.to_string()),
            Span::styled(
                match missing {
                    true => format!("   ⚠ {}", target.status),
                    false => format!("   {}", target.status),
                },
                if missing {
                    theme::warn()
                } else {
                    theme::muted()
                },
            ),
        ];
        out.row(spans, focused == target.row, row_index(target.row));
        let mut text: Vec<(String, Style)> = target
            .steps
            .iter()
            .map(|step| (step.clone(), Style::default()))
            .collect();
        text.extend(warned(&target.kit_warning));
        out.explain(focused == target.row, target.title, &text);
    }

    let close = row_index(ConnectRow::Cancel);
    let buttons = [("Close", focused == ConnectRow::Cancel, Look::Plain, close)];
    framed(
        frame,
        area,
        &format!("Connect to {}", view.label),
        out,
        &buttons,
    )
}

// --- the start folder --------------------------------------------------------------------------

/// Which directory inside the workspace the agent opens in.
/// The whole workspace stays mounted: the picker moves only where the agent opens.
pub(super) fn render_start_dir(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::StartDir(form) = &app.screen else {
        return Hits::new();
    };
    // Only an existing sandbox has a recorded start folder; a new one's choice is in `chosen()`.
    let current = match &form.target {
        crate::app::StartDirTarget::Sandbox(name) => app
            .sandboxes
            .iter()
            .find(|s| s.name == *name)
            .and_then(|s| s.start_dir.clone()),
        crate::app::StartDirTarget::New(create) => create.workspace.start_dir.clone(),
        crate::app::StartDirTarget::HostFolder(_) => {
            return render_host_folder_pick(frame, form, area)
        }
    };
    let label = form.label.clone();
    let rows = form.rows();
    let focused = form.focused();
    let row_index = |row: StartDirRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    let chosen = form.chosen();

    let mut out = Lines::new(area);
    out.section_with("Mounted", "the whole folder — this screen never changes it");
    out.note(format!("     {}", model::tilde(&form.root)));
    out.para(
        "     Every project under it stays in the sandbox and reachable by path, whichever \
         folder is picked below.",
    );

    out.blank();
    out.section_with(
        "Opens in",
        "the agent's working directory, and its project root",
    );
    out.note(format!(
        "     {}",
        crate::startdir::describe(&form.root, chosen.as_deref())
    ));
    out.para(match chosen.is_some() {
        true => {
            "     Claude Code / OpenCode starts here and reads this as the project it was \
                 opened in — its CLAUDE.md, its settings, its session history."
        }
        false => {
            "     The workspace root — where sbx starts an agent by default. Pick a folder \
                  below to open somewhere inside it instead."
        }
    });
    if form.unchanged(current.as_deref()) && current.is_some() {
        out.note("     Already this sandbox's start folder.");
    }

    out.blank();
    browse_rows(
        &mut out,
        form,
        "     (no subfolders — this workspace is a single project)",
    );

    let buttons = [
        (
            StartDirRow::Start,
            "Start here",
            "Stores it and hands the terminal over now. Nothing is rebuilt — a start folder is an \
             argument to the command that opens the session, not something baked into the \
             container.",
        ),
        (
            StartDirRow::Use,
            "Use, don't start",
            "Stores it and stays here. ↵ and x on the sandbox open there from now on, until this \
             screen says otherwise.",
        ),
        (
            StartDirRow::Cancel,
            "Cancel",
            "Leaves the sandbox exactly as it is.",
        ),
    ];
    for (row, label, text) in buttons {
        out.explain_lines(focused == row, label, &[text.to_string()]);
    }
    let buttons: Vec<(&str, bool, bool, usize)> = buttons
        .iter()
        .map(|(row, label, _)| (*label, focused == *row, false, row_index(*row)))
        .collect();
    framed(
        frame,
        area,
        &format!("Start folder — {label}"),
        out,
        &looks(&buttons),
    )
}

/// The picker's `..` and folder rows, under their heading. `at_root` is the note for a root
/// level without subfolders.
fn browse_rows(out: &mut Lines, form: &crate::app::StartDirForm, at_root: &str) {
    let rows = form.rows();
    let focused = form.focused();
    let row_index = |row: StartDirRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    out.section_with("Browse", "↵ or → goes in · ← comes back out");
    if !form.rel.as_os_str().is_empty() {
        let parent = form
            .rel
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| {
                form.root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| model::tilde(&form.root))
            });
        out.row(
            vec![
                Span::raw("   "),
                Span::raw("←  .."),
                Span::styled(format!("   back to {parent}"), theme::muted()),
            ],
            focused == StartDirRow::Up,
            row_index(StartDirRow::Up),
        );
    } else if form.widens() {
        let parent = form
            .root
            .parent()
            .map(crate::model::tilde)
            .unwrap_or_default();
        out.row(
            vec![
                Span::raw("   "),
                Span::raw("←  .."),
                Span::styled(format!("   mount {parent} instead"), theme::muted()),
            ],
            focused == StartDirRow::Up,
            row_index(StartDirRow::Up),
        );
    }
    if form.dirs.is_empty() {
        out.note(match form.rel.as_os_str().is_empty() {
            true => at_root,
            false => "     (nothing below this one)",
        });
    }
    // Child counts cost one readdir per row, so a level of over 200 folders goes without them.
    let count_children = form.dirs.len() <= 200;
    for (i, name) in form.dirs.iter().enumerate() {
        let deeper = if count_children {
            crate::startdir::children(&form.here().join(name)).len()
        } else {
            0
        };
        let mut spans = vec![Span::raw("   "), Span::raw(format!("→  {name}"))];
        if deeper > 0 {
            spans.push(Span::styled(
                match deeper {
                    1 => "   1 folder inside".to_string(),
                    n => format!("   {n} folders inside"),
                },
                theme::muted(),
            ));
        }
        out.row(
            spans,
            focused == StartDirRow::Dir(i),
            row_index(StartDirRow::Dir(i)),
        );
        // Where the folder is, whole: a long name is cut on its row.
        let path = match form.target {
            crate::app::StartDirTarget::HostFolder(_) => model::tilde(&form.here().join(name)),
            _ => crate::startdir::describe(&form.root, Some(form.rel.join(name).as_path())),
        };
        out.explain_lines(focused == StartDirRow::Dir(i), name.clone(), &[path]);
    }
}

/// The host folders screen's picker: any folder on the host, for its folder field.
fn render_host_folder_pick(frame: &mut Frame, form: &crate::app::StartDirForm, area: Rect) -> Hits {
    let rows = form.rows();
    let focused = form.focused();
    let row_index = |row: StartDirRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    let mut out = Lines::new(area);
    out.section_with("Folder", "what Use puts in the field");
    out.note(format!("     {}", model::tilde(&form.here())));
    out.para(
        "     Never your home folder itself, a folder above it or one that holds credentials. \
         Hidden folders are not listed: type their path instead.",
    );
    out.blank();
    browse_rows(&mut out, form, "     (no subfolders)");
    let buttons = [
        (
            StartDirRow::Use,
            "Use",
            "Puts this folder in the folder field of the host folders screen. Nothing is \
             saved until Save there.",
        ),
        (
            StartDirRow::Cancel,
            "Cancel",
            "Back to the host folders screen, the field as it was.",
        ),
    ];
    for (row, label, text) in buttons {
        out.explain_lines(focused == row, label, &[text.to_string()]);
    }
    let buttons: Vec<(&str, bool, bool, usize)> = buttons
        .iter()
        .map(|(row, label, _)| (*label, focused == *row, false, row_index(*row)))
        .collect();
    framed(frame, area, "Pick a host folder", out, &looks(&buttons))
}

/// Where a row of buttons landed, so each one gets its own click box.
struct ButtonRow {
    line: usize,
    /// (x offset inside the panel, width, form row index)
    spans: Vec<(u16, u16, usize)>,
}

impl ButtonRow {
    fn hits(&self, area: Rect, scroll: u16) -> Hits {
        let Some(y) = (self.line as u16)
            .checked_sub(scroll)
            .map(|d| d + area.y + 1)
        else {
            return Hits::new();
        };
        if y >= area.bottom().saturating_sub(1) {
            return Hits::new();
        }
        self.spans
            .iter()
            .map(|(x, width, index)| {
                (
                    Rect {
                        x: area.x + 1 + x,
                        y,
                        width: *width,
                        height: 1,
                    },
                    Hit::FormRow(*index),
                )
            })
            .collect()
    }
}

/// A form's line of buttons, given as `(label, focused, danger, index)`, with their looks. The
/// first of several is the primary one (every form puts its main action first), unless a danger.
fn looks<'b>(buttons: &[(&'b str, bool, bool, usize)]) -> Vec<(&'b str, bool, Look, usize)> {
    let several = buttons.len() > 1;
    buttons
        .iter()
        .enumerate()
        .map(|(n, (label, focused, danger, index))| {
            let look = match (*danger, n == 0 && several) {
                (true, _) => Look::Danger,
                (false, true) => Look::Primary,
                (false, false) => Look::Plain,
            };
            (*label, *focused, look, *index)
        })
        .collect()
}

/// A line of buttons, each with its given look (see [`looks`]), and where each one landed.
fn button_line(out: &mut Lines<'_>, buttons: &[(&str, bool, Look, usize)]) -> ButtonRow {
    let mut spans = Vec::new();
    let mut boxes = Vec::new();
    let mut x = 0u16;
    for (label, focused, look, index) in buttons {
        if !spans.is_empty() {
            spans.push(Span::raw("  "));
            x += 2;
        }
        let width = label.chars().count() as u16 + 4;
        spans.extend(button(label, *focused, *look));
        boxes.push((x, width, *index));
        x += width;
    }
    let line = out.lines.len();
    out.lines.push(Line::from(spans));
    ButtonRow { line, spans: boxes }
}

/// The rows a form keeps above its help strip, whatever the strip would like: a strip that
/// leaves fewer is cut, the cut marked with `…`.
const MIN_BODY: u16 = 5;

/// Draw a form in a frame like the create wizard's: `out`'s lines at the top, scrolled to the
/// cursor's row, then the help strip ([`Lines::explain`]); the buttons are the panel's last line.
fn framed(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    out: Lines<'_>,
    buttons: &[(&str, bool, Look, usize)],
) -> Hits {
    frame.render_widget(panel(title, true), area);
    let mut hits = Hits::new();
    // A line of rows, the buttons and the borders, at the very least.
    if area.height < 4 || area.width < 8 {
        return hits;
    }
    let (x, width) = (area.x + 2, area.width - 4);
    let top = area.y + 1;
    let bar_y = area.bottom() - 2;
    let room = bar_y - top;
    let strip = match out.help_need {
        0 => 0,
        need => (need as u16).min(room.saturating_sub(1 + MIN_BODY)),
    };
    // The strip's rule sits under the last row; rows that do not fit fill the room and scroll.
    let total = out.lines.len();
    let room = room.saturating_sub(1 + strip).max(1);
    let body = match strip {
        0 => Rect::new(x, top, width, room),
        _ => Rect::new(x, top, width, (total as u16).clamp(1, room)),
    };

    let height = body.height as usize;
    let scroll = match buttons.iter().any(|(_, focused, ..)| *focused) {
        true => total.saturating_sub(height) as u16,
        false => scroll_for(out.focus_line.map(|l| (l, l)), total, body.height + 2),
    };
    hits.extend(out.hits(
        Rect::new(area.x, top - 1, area.width, body.height + 2),
        scroll,
    ));
    let help = out.help;
    let hidden = Hidden {
        above: scroll as usize,
        below: total.saturating_sub(scroll as usize + height),
    };
    frame.render_widget(Paragraph::new(out.lines).scroll((scroll, 0)), body);
    // The rule under the rows says what they hide, or the blank line where there is no strip.
    let at = Rect::new(x, body.bottom(), width, strip + 1);
    help_strip(frame, help.filter(|_| strip > 0), hidden, at);

    let mut bar = Lines::new(area);
    let drawn = button_line(&mut bar, buttons);
    hits.extend(drawn.hits(Rect::new(area.x, bar_y - 1, area.width, 3), 0));
    frame.render_widget(Paragraph::new(bar.lines), Rect::new(x, bar_y, width, 1));
    hits
}

/// How many lines of a form's rows are scrolled out of view, above and below.
#[derive(Debug, Clone, Copy)]
struct Hidden {
    above: usize,
    below: usize,
}

impl Hidden {
    /// `↑ 3  ↓ 5 more`, or nothing when every row is in view.
    fn hint(self) -> String {
        let mut parts = Vec::new();
        if self.above > 0 {
            parts.push(format!("↑ {}", self.above));
        }
        if self.below > 0 {
            parts.push(format!("↓ {} more", self.below));
        }
        parts.join("  ")
    }
}

/// The help strip in `area`: a rule naming the row under the cursor and counting the hidden lines,
/// then what [`Lines::explain`] said about the row, cut to the strip.
fn help_strip(frame: &mut Frame, help: Option<Strip>, hidden: Hidden, area: Rect) {
    let width = area.width as usize;
    let (title, mut body) = match help {
        Some(Strip { title, lines }) => (title, lines),
        None => (String::new(), Vec::new()),
    };
    let hint = hidden.hint();
    let tail = match hint.is_empty() {
        true => Vec::new(),
        false => vec![Span::styled(format!(" {hint} "), theme::muted())],
    };
    let tail_width: usize = tail.iter().map(|s| s.content.chars().count()).sum();
    let rule = match (title.is_empty(), area.height > 1) {
        // No strip: a blank line, the count at its end.
        (_, false) => {
            let mut spans = vec![Span::raw(" ".repeat(width.saturating_sub(tail_width)))];
            spans.extend(tail);
            Line::from(spans)
        }
        (true, true) => {
            let mut spans = vec![Span::styled(
                "─".repeat(width.saturating_sub(tail_width)),
                theme::border(),
            )];
            spans.extend(tail);
            Line::from(spans)
        }
        (false, true) => {
            let title = super::clip(&title, width.saturating_sub(6 + tail_width));
            let used = title.chars().count() + 3 + tail_width;
            let mut spans = vec![
                Span::styled("─ ", theme::border()),
                Span::styled(title, Style::default().bold()),
                Span::styled(
                    format!(" {}", "─".repeat(width.saturating_sub(used))),
                    theme::border(),
                ),
            ];
            spans.extend(tail);
            Line::from(spans)
        }
    };
    let room = area.height.saturating_sub(1) as usize;
    if body.len() > room {
        body.truncate(room);
        if let Some(last) = body.last_mut() {
            let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
            let style = last.spans.first().map(|s| s.style).unwrap_or_default();
            *last = Line::from(Span::styled(
                super::clip(&format!("{text} …"), width),
                style,
            ));
        }
    }
    let mut lines = vec![rule];
    lines.extend(body);
    frame.render_widget(Paragraph::new(lines), area);
}

// --- skills ----------------------------------------------------------------------------------

/// The shared agent-skills store: what it holds, who mounts it, and the commands that change it.
/// Rows are drawn in [`App::skill_rows`] order, so a click box's index is the key handler's.
pub(super) fn render_skills(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Skills(form) = &app.screen else {
        return Hits::new();
    };
    let rows = app.skill_rows();
    let focus = form.focus.min(rows.len().saturating_sub(1));
    let known = app.skills.is_some();

    let mut out = Lines::new(area);
    match app.skills_store_path() {
        Some(store) => out.note(format!("Store: {}", model::tilde(&store))),
        None => out.note("Store: not reported by sbx"),
    }
    out.note(format!(
        "Mounted into every sandbox that shares it ({}), so a skill added, replaced",
        match app.skills_sharers() {
            0 => "none here yet".to_string(),
            1 => "1 here".to_string(),
            n => format!("{n} here"),
        }
    ));
    out.note("or removed here is what each of them loads at its next agent session.");
    for line in app.skills_trust_lines() {
        out.note(line);
    }
    if !known {
        out.blank();
        out.note("reading `sbx skills ls`…");
    }

    let mut section: Option<&'static str> = None;
    let mut close = rows.len();
    for (i, row) in rows.iter().enumerate() {
        if let SkillRow::Close = row {
            close = i;
            continue;
        }
        let heading = App::skill_heading(row);
        if heading != section {
            out.blank();
            // An empty store has no skill rows to put the first heading over, so it is said.
            if heading == Some("Change the store")
                && !rows.iter().any(|r| matches!(r, SkillRow::Skill(_)))
            {
                out.section("Installed skills");
                out.note(if known {
                    "  nothing in the store yet — import this host's, or add from a repository"
                } else {
                    "  …"
                });
                out.blank();
            }
            if let Some(title) = heading {
                out.section(title);
            }
            section = heading;
        }
        out.row(skill_row_spans(app, row), focus == i, i);
        let (name, _) = app.skill_view(row);
        out.explain_lines(focus == i, name, &app.skill_notes(row));
    }
    let buttons = [("Close", focus == close, Look::Plain, close)];
    framed(frame, area, "Shared agent skills", out, &buttons)
}

/// `text` in a fixed column `width` cells wide, cut two cells short of it, so the next column
/// keeps its gap.
fn column(text: &str, width: usize) -> String {
    format!("{:<width$}", super::clip(text, width.saturating_sub(2)))
}

/// One line per row: the skill, or the command, and in a muted column what it reaches for.
fn skill_row_spans(app: &App, row: &SkillRow) -> Vec<Span<'static>> {
    const NAME: usize = 30;
    let (name, note) = app.skill_view(row);
    vec![
        Span::raw("   "),
        Span::styled(
            column(&name, NAME),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(note, theme::muted()),
    ]
}

// --- repo skills -----------------------------------------------------------------------------

/// The Repo skills screen: each skill repo, its "all skills" row and a row per skill it holds, in
/// [`crate::repo_skills_ui::RepoSkillsForm::rows`] order, which the key handler uses too.
pub(super) fn render_repo_skills(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::RepoSkills(form) = &app.screen else {
        return Hits::new();
    };
    let rows = form.rows();
    let focus = form.focus.min(rows.len().saturating_sub(1));
    let title = match &form.scope {
        RepoScope::Global => "Repo skills · all sandboxes".to_string(),
        RepoScope::Sandbox(name) => {
            let label = app
                .sandboxes
                .iter()
                .find(|sb| &sb.name == name)
                .map_or(name.as_str(), |sb| sb.label.as_str());
            format!("Repo skills · {label}")
        }
    };

    let mut out = Lines::new(area);
    match &form.scope {
        RepoScope::Global => out.para(
            "Ticked skills go into every sandbox, existing and new. Existing sandboxes get a \
             change at their next start, new ones at their first.",
        ),
        RepoScope::Sandbox(name) => out.para(format!(
            "Skills for {name} alone, on top of the global ones (locked here — change them in \
             K → Repo skills for all sandboxes). A save applies at its next start, or now with \
             Sync now."
        )),
    }
    if let Some(why) = &form.error {
        out.blank();
        out.warn(format!("The selection cannot be read: {why}"));
        out.warn("Nothing can be saved until the file is fixed or removed; it is left as it is.");
    } else if form.repos.is_empty() {
        out.blank();
        out.note("No skill repo is configured. Add one with `aisbx skill-repo add SOURCE`,");
        out.note("then `aisbx skill-repo fetch`.");
    }

    let mut buttons: Vec<(&str, bool, bool, usize)> = Vec::new();
    let mut shown_repo: Option<&str> = None;
    for (i, row) in rows.iter().enumerate() {
        let repo_id = match row {
            RepoSkillRow::All(repo) | RepoSkillRow::Skill { repo, .. } => Some(repo.as_str()),
            _ => None,
        };
        // A repo's heading, then (after its rows) its fetch hint and skipped folders.
        if repo_id != shown_repo {
            let find = |id: Option<&str>| id.and_then(|id| form.repos.iter().find(|r| r.id == id));
            if let Some(done) = find(shown_repo) {
                repo_footer(&mut out, done);
            }
            if let Some(repo) = find(repo_id) {
                out.blank();
                let commit = match &repo.commit {
                    Some(c) => format!("at {}", &c[..c.len().min(12)]),
                    None => "not fetched".to_string(),
                };
                out.lines.push(Line::from(vec![
                    Span::styled(repo.id.clone(), theme::muted().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("   {}  ·  {commit}", repo.source), theme::muted()),
                ]));
            } else {
                out.blank();
            }
            shown_repo = repo_id;
        }
        match row {
            RepoSkillRow::Save => buttons.push(("Save", focus == i, false, i)),
            RepoSkillRow::Close => buttons.push(("Close", focus == i, false, i)),
            _ => {
                out.row(repo_skill_spans(form, row), focus == i, i);
                let title = match row {
                    RepoSkillRow::All(_) => "all skills".to_string(),
                    RepoSkillRow::Skill { name, .. } => name.clone(),
                    _ => "Sync now".to_string(),
                };
                out.explain_lines(focus == i, title, &form.notes(row));
            }
        }
    }
    if form.dirty {
        out.blank();
        out.warn("Not saved yet.");
    }
    framed(frame, area, &title, out, &looks(&buttons))
}

/// Under a repo's rows: why it has no cache (with the fetch hint) and the folders it skipped.
fn repo_footer(out: &mut Lines<'_>, repo: &crate::repo_skills_ui::RepoView) {
    if let Some(problem) = &repo.problem {
        out.note(format!("     {problem}"));
    }
    for (name, why) in &repo.skipped {
        out.note(format!(" [-] {}   skipped: {why}", name.escape_debug()));
    }
}

/// One tick row: the box, the name, and `global` where a global pick locks it.
fn repo_skill_spans(
    form: &crate::repo_skills_ui::RepoSkillsForm,
    row: &RepoSkillRow,
) -> Vec<Span<'static>> {
    let ticked = form.ticked(row);
    let (name, locked) = match row {
        RepoSkillRow::All(repo) => ("all skills".to_string(), form.locked(repo, None)),
        RepoSkillRow::Skill { name, locked, .. } => (name.clone(), *locked),
        RepoSkillRow::SyncNow => {
            return vec![
                Span::raw("   "),
                Span::styled(
                    "Sync now".to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled("   else it applies at the next start", theme::muted()),
            ];
        }
        RepoSkillRow::Save | RepoSkillRow::Close => return Vec::new(),
    };
    let tick = if ticked { " [x] " } else { " [ ] " };
    let mut spans = vec![
        Span::styled(
            tick,
            if locked {
                theme::muted()
            } else {
                Style::default()
            },
        ),
        Span::styled(
            name,
            if locked {
                theme::muted()
            } else {
                Style::default()
            },
        ),
    ];
    if locked {
        spans.push(Span::styled("   global", theme::muted()));
    }
    spans
}

// --- secrets ---------------------------------------------------------------------------------

/// Everything stored on this host, and what each one is shared with.
/// Rows are drawn in [`App::secret_rows`] order, so a click box's index is the key handler's.
pub(super) fn render_secrets(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Secrets(form) = &app.screen else {
        return Hits::new();
    };
    let rows = app.secret_rows();
    let focus = form.focus.min(rows.len().saturating_sub(1));
    let known = app.secrets.is_some();

    let mut out = Lines::new(area);
    out.para(
        "Values live in the host keychain and are shown masked, exactly as sbx prints them. A \
         sandbox only ever sees a sentinel or a placeholder — the proxy swaps in the rest.",
    );
    if !known {
        out.blank();
        out.note("reading `sbx secret ls`…");
    }

    let mut section: Option<&'static str> = None;
    let mut close = rows.len();
    for (i, row) in rows.iter().enumerate() {
        if let SecretRow::Close = row {
            close = i;
            continue;
        }
        let heading = App::secret_heading(row);
        if heading != section {
            out.blank();
            if let Some(title) = heading {
                out.section(title);
            }
            section = heading;
        }
        out.row(secret_row_spans(app, row), focus == i, i);
        let name = app.secret_view(row).name;
        out.explain_lines(focus == i, name, &app.secret_notes(row));
    }
    // Custom secrets stored by someone else: listed, but never touched from here.
    let foreign = app.foreign_secret_lines();
    if !foreign.is_empty() {
        out.blank();
        out.section("Other custom secrets — not aisbx's");
        for line in foreign {
            out.note(format!("  {line}"));
        }
    }
    let buttons = [("Close", focus == close, Look::Plain, close)];
    framed(frame, area, "Stored secrets", out, &buttons)
}

/// One credential's line: what it is, what sbx has stored for it, and who it belongs to.
fn secret_row_spans(app: &App, row: &SecretRow) -> Vec<Span<'static>> {
    let view = app.secret_view(row);
    // Fixed columns that line up down the screen: 26 holds a 24-character name and its gap.
    const NAME: usize = 26;
    const VALUE: usize = 20;
    let value = match (&view.stored, view.empty) {
        (Some(masked), _) => {
            Span::styled(column(masked, VALUE), Style::default().fg(theme::RUNNING))
        }
        (None, Empty::Unknown) => Span::styled(format!("{:<VALUE$}", "…"), theme::muted()),
        (None, Empty::SignedIn) => Span::styled(
            format!("{:<VALUE$}", "signed in"),
            Style::default().fg(theme::RUNNING),
        ),
        (None, Empty::NotSignedIn) => {
            Span::styled(format!("{:<VALUE$}", "not signed in"), theme::muted())
        }
        (None, Empty::NotListed) => {
            Span::styled(format!("{:<VALUE$}", "not listed by sbx"), theme::muted())
        }
        (None, Empty::NotStored) => {
            Span::styled(format!("{:<VALUE$}", "⚠ not stored"), theme::warn())
        }
    };
    vec![
        Span::raw("   "),
        Span::styled(
            column(&view.name, NAME),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        value,
        Span::styled(view.note, theme::muted()),
    ]
}

/// Requests that sandboxes hold for approval, grouped by sandbox, and what answering one does.
/// An answer goes straight to sbx: the row is the question, so there is no second confirmation.
pub(super) fn render_approvals(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Approvals(form) = &app.screen else {
        return Hits::new();
    };
    let rows = app.approval_rows();
    let focus = form.focus.min(rows.len().saturating_sub(1));

    let mut out = Lines::new(area);
    if rows.len() == 1 {
        out.section("Nothing is waiting");
        out.para(format!("     {}", crate::app::APPROVALS_EMPTY));
    }

    let mut current: Option<String> = None;
    for (i, row) in rows.iter().enumerate() {
        match row {
            ApprovalRow::Request(id) => {
                let Some(request) = app.approval(id) else {
                    continue;
                };
                if current.as_deref() != Some(request.sandbox.as_str()) {
                    if current.is_some() {
                        out.blank();
                    }
                    let label = app.sandbox_label(&request.sandbox);
                    out.lines.push(Line::from(vec![
                        Span::styled(label.clone(), theme::muted().add_modifier(Modifier::BOLD)),
                        Span::styled(
                            match label == request.sandbox {
                                true => String::new(),
                                false => format!("   {}", request.sandbox),
                            },
                            theme::muted(),
                        ),
                    ]));
                    current = Some(request.sandbox.clone());
                }
                const TITLE: usize = 34;
                out.row(
                    vec![
                        Span::styled(" ⚑ ", theme::warn()),
                        Span::styled(
                            format!("{:<TITLE$}", super::clip(&request.title, TITLE)),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(request.reason(), theme::muted()),
                    ],
                    focus == i,
                    i,
                );
                out.explain_lines(focus == i, request.title.clone(), &app.approval_notes(id));
            }
            // What every answer here does, under the one button: the requests come first.
            ApprovalRow::Close => out.explain_lines(
                focus == i,
                "Close",
                &[crate::app::APPROVALS_NOTE.to_string()],
            ),
        }
    }
    let close = rows.len().saturating_sub(1);
    let buttons = [("Close", focus == close, Look::Plain, close)];
    framed(
        frame,
        area,
        "Network requests waiting for approval",
        out,
        &buttons,
    )
}

pub(super) fn render_kits(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Kits(form) = &app.screen else {
        return Hits::new();
    };
    let catalog = &app.catalog;
    let rows = form.rows(catalog);
    let focused = rows[form.focus.min(rows.len() - 1)];
    let row_index = |row: KitsRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    let sandbox = app.sandboxes.iter().find(|s| s.name == form.sandbox);

    let mut out = Lines::new(area);
    if let Some(sb) = sandbox {
        let backend = sb
            .backend
            .as_deref()
            .and_then(|b| catalog.backend(b))
            .map(|b| b.kit.display_name.clone())
            .unwrap_or_else(|| sb.agent.clone());
        // Like the Overview's `agent` row; the gloss goes under it where it does not fit beside it.
        let mut line = vec![
            Span::styled("agent  ", theme::muted()),
            Span::styled(backend, Style::default().fg(theme::agent_color(&sb.agent))),
        ];
        let width = Line::from(line.clone()).width() + 3 + AGENT_FIXED.chars().count();
        if width <= out.width as usize {
            line.push(Span::styled(format!("   {AGENT_FIXED}"), theme::muted()));
            out.lines.push(Line::from(line));
        } else {
            out.lines.push(Line::from(line));
            out.note(AGENT_FIXED);
        }
        out.blank();
    }

    // The wizard's two blocks, in core's order (`KitsForm::addon_order`), so a kit is found where
    // it was picked. The marker and the change get one column each, after the longest name.
    let column = catalog
        .addons
        .iter()
        .map(|a| a.kit.display_name.chars().count())
        .max()
        .unwrap_or(0);
    let order: Vec<usize> = rows
        .iter()
        .filter_map(|row| match row {
            KitsRow::Addon(i) => Some(*i),
            _ => None,
        })
        .collect();
    let hosts = order
        .iter()
        .take_while(|i| crate::model::is_host_folder_kit(&catalog.addons[**i].kit.name))
        .count();
    for (n, &i) in order.iter().enumerate() {
        if n == 0 && hosts > 0 {
            out.section_with("Host caches", HOST_CACHES_GLOSS);
        }
        if n == hosts {
            out.section_with("Additional kits", KITS_GLOSS);
        }
        let addon = &catalog.addons[i];
        let was = form.original[i];
        let now = form.addons[i];
        let change = match (was, now) {
            (false, true) => Some(("+ add", theme::RUNNING)),
            (true, false) => Some(("− remove", theme::DANGER)),
            _ => None,
        };
        out.row(
            addon_spans(
                addon.kit.name.as_str(),
                &addon.kit.display_name,
                now,
                Some(Marker::Quiet(&format!(
                    "{:<width$}",
                    match addon.create_time_only() {
                        true => CREATE_TIME,
                        false => "",
                    },
                    width = CREATE_TIME.chars().count()
                ))),
                change,
                column,
                out.width as usize,
            ),
            focused == KitsRow::Addon(i),
            row_index(KitsRow::Addon(i)),
        );
    }
    // What each row is about, worded by core: a kit's sentence and what ticking it costs here.
    for (n, row) in rows.iter().enumerate() {
        let help = form.help(catalog, *row);
        out.explain_lines(n == row_index(focused), help.title, &[help.text]);
    }
    // Keyed on the agent sbx reports: it decides whether anything goes through the proxy.
    let headroom = catalog.addons.iter().position(|a| a.kit.name == "headroom");
    if let (Some(sb), Some(i)) = (sandbox, headroom) {
        if form.addons[i] {
            out.warn(format!("     {}", crate::kits::headroom_warning(&sb.agent)));
        }
    }

    if form.models_offered {
        out.blank();
        let all: Vec<usize> = (0..catalog.models.len()).collect();
        model_section(
            &mut out,
            ModelList {
                title: "Claude models",
                gloss: "optional — this gateway may not serve Claude Code's own defaults",
                slots: &Slot::ALL,
                models: &catalog.models,
                offered: &all,
                source: Some(&catalog.models_source),
            },
            &form.models,
            form.model_pick,
            focused,
            &row_index,
            KitsRow::Model,
            KitsRow::ModelDefault,
            KitsRow::ModelPick,
            true,
        );
    }

    out.blank();
    let removing = form.removing();
    // What Apply does is core's decision (`KitsForm::effect`), the one `submit_kits` acts on.
    let other_checkout = sandbox.is_some_and(|sb| sb.other_checkout.is_some());
    for line in form.effect(catalog, other_checkout).note(form.history()) {
        out.para(line);
    }

    let buttons = [
        (
            "Apply",
            focused == KitsRow::Apply,
            removing,
            row_index(KitsRow::Apply),
        ),
        (
            "Cancel",
            focused == KitsRow::Cancel,
            false,
            row_index(KitsRow::Cancel),
        ),
    ];
    let title = sandbox
        .map(|s| format!("Kits — {}", s.label))
        .unwrap_or_else(|| "Kits".to_string());
    framed(frame, area, &title, out, &looks(&buttons))
}

// --- the model aliases, worded once ------------------------------------------------------------

/// What [`model_section`] lists: its heading, the slots, and the catalog's models with the
/// indices of the ones on offer.
struct ModelList<'a> {
    title: &'a str,
    gloss: &'a str,
    slots: &'a [Slot],
    models: &'a [Model],
    offered: &'a [usize],
    /// Where the list came from, said after the title; `None` where a row above already says it.
    source: Option<&'a crate::models::Source>,
}

/// The model slots section, with the open slot's model list expanded under it. The create form
/// and the kits editor share it; the three constructors give each its own row type.
#[allow(clippy::too_many_arguments)]
fn model_section<R: Copy + PartialEq>(
    out: &mut Lines<'_>,
    list: ModelList<'_>,
    over: &Overrides,
    open: Option<Slot>,
    focused: R,
    index: &dyn Fn(R) -> usize,
    alias: fn(Slot) -> R,
    default: fn(Slot) -> R,
    pick: fn(Slot, usize) -> R,
    notes: bool,
) {
    let models = list.models;
    let title = match list.source {
        Some(source) => format!("{} · {}", list.title, source.label()),
        None => list.title.to_string(),
    };
    let gloss = list.gloss;
    let mut heading = vec![Span::styled(
        title.clone(),
        theme::muted().add_modifier(Modifier::BOLD),
    )];
    if title.chars().count() + 3 + gloss.chars().count() <= out.width as usize {
        heading.push(Span::styled(format!("   {gloss}"), theme::muted()));
    }
    out.lines.push(Line::from(heading));
    let width = list
        .slots
        .iter()
        .map(|s| s.label().chars().count())
        .max()
        .unwrap_or(0)
        + 2;
    for slot in list.slots.iter().copied() {
        let open_here = open == Some(slot);
        let mut spans = vec![
            Span::styled(
                format!(" {} ", if open_here { "▾" } else { "▸" }),
                theme::muted(),
            ),
            Span::styled(format!("{:<width$}", slot.label()), Style::default().bold()),
        ];
        spans.extend(pinned_spans(models, over.get(slot)));
        out.row(spans, focused == alias(slot), index(alias(slot)));
        if notes && focused == alias(slot) && !open_here {
            out.note(format!("        {}", slot.hint()));
        }
        if !open_here {
            continue;
        }
        out.row(
            model_spans(over.get(slot).is_none(), slot.unset(), ""),
            focused == default(slot),
            index(default(slot)),
        );
        for i in list.offered.iter().copied() {
            let Some(model) = models.get(i) else {
                continue;
            };
            out.row(
                model_spans(
                    over.get(slot) == Some(model.id.as_str()),
                    &model.name,
                    &model.id,
                ),
                focused == pick(slot, i),
                index(pick(slot, i)),
            );
        }
    }
}

/// What an alias is pinned to today, on its own row.
fn pinned_spans(models: &[Model], pinned: Option<&str>) -> Vec<Span<'static>> {
    let Some(id) = pinned else {
        return vec![Span::styled("(default)", theme::muted())];
    };
    // An id the catalog has lost is shown as-is: it is the value the container carries.
    let mut spans = vec![Span::styled(
        crate::models::display_name(models, id).unwrap_or_else(|| id.to_string()),
        Style::default().fg(theme::RUNNING).bold(),
    )];
    if crate::models::display_name(models, id).is_some() {
        spans.push(Span::styled(format!("   {id}"), theme::muted()));
    }
    spans
}

/// One entry in an alias's model list, indented under it.
fn model_spans(selected: bool, name: &str, note: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if selected { "    ◉ " } else { "    ○ " },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{name:<20}"),
            match selected {
                true => Style::default().fg(theme::RUNNING).bold(),
                false => Style::default(),
            },
        ),
        Span::styled(note.to_string(), theme::muted()),
    ]
}

/// One entry in the pick list under the account radio, indented under it.
fn token_spans(selected: bool, name: &str, note: String) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if selected { "    ◉ " } else { "    ○ " },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{name:<14}"),
            match selected {
                true => Style::default().fg(theme::RUNNING).bold(),
                false => Style::default(),
            },
        ),
        Span::styled(note, theme::muted()),
    ]
}

/// One of a pair of mutually exclusive choices, laid out like the backend rows above it.
fn radio_spans(selected: bool, label: &str, note: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if selected { " ◉ " } else { " ○ " },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{label:<16}"),
            if selected {
                Style::default().bold()
            } else {
                Style::default()
            },
        ),
        Span::styled(format!(" — {note}"), theme::muted()),
    ]
}

// --- the account choice, worded once ---------------------------------------------------------

/// The `/login` half of the radio, marked recommended.
fn login_radio(selected: bool) -> Vec<Span<'static>> {
    let mut spans = radio_spans(selected, "/login (OAuth)", "");
    spans.pop();
    spans.push(Span::styled(
        format!(" — {}", crate::create::LOGIN_TAG),
        Style::default()
            .fg(theme::RUNNING)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" · {}", crate::create::LOGIN_HINT),
        theme::muted(),
    ));
    spans
}

/// The setup-token half: a second subscription, where the name is the account. Selected, it says
/// what to do next, which depends on whether this host has a token yet.
fn token_radio(selected: bool, stored: usize) -> Vec<Span<'static>> {
    radio_spans(
        selected,
        "setup token",
        crate::create::token_hint(selected, stored),
    )
}

/// The auth editor: which Anthropic account one existing sandbox is on. The panel leads with
/// what applying the selection would do, since that differs by direction.
pub(super) fn render_auth(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::AuthEdit(form) = &app.screen else {
        return Hits::new();
    };
    let rows = form.rows();
    let focused = rows[form.focus.min(rows.len() - 1)];
    let row_index = |row: AuthRow| rows.iter().position(|r| *r == row).unwrap_or(0);

    let mut out = Lines::new(area);
    out.note(format!("sbx name: {}", form.sandbox));
    out.blank();
    out.section("Now");
    let users = form
        .current
        .token_name()
        .map(|name| app.token_users(name))
        .unwrap_or(0);
    out.lines
        .push(Line::from(current_spans(&form.current, users)));

    out.blank();
    out.section("Change to");
    out.row(
        login_radio(!form.account.on_token()),
        focused == AuthRow::Oauth,
        row_index(AuthRow::Oauth),
    );
    if !form.account.on_token() {
        for note in account_notes(&form.account) {
            out.para(format!("      {note}"));
        }
    }
    out.row(
        token_radio(form.account.on_token(), form.tokens.len()),
        focused == AuthRow::UseToken,
        row_index(AuthRow::UseToken),
    );
    if form.account.on_token() {
        for (i, token) in form.tokens.iter().enumerate() {
            out.row(
                token_spans(
                    form.account == AccountChoice::Named(token.clone()),
                    token,
                    crate::create::token_users(app.token_users(token)),
                ),
                focused == AuthRow::TokenPick(i),
                row_index(AuthRow::TokenPick(i)),
            );
        }
        out.row(
            token_spans(
                form.account == AccountChoice::New,
                "new token…",
                "paste a new token and name it".into(),
            ),
            focused == AuthRow::TokenNew,
            row_index(AuthRow::TokenNew),
        );

        let rows = form.rows();
        if rows.contains(&AuthRow::Name) {
            let label = "        name  ";
            let mut name_spans = vec![Span::styled(label, theme::muted())];
            name_spans.extend(input_spans(&form.token_name, focused == AuthRow::Name));
            out.row(
                name_spans,
                focused == AuthRow::Name,
                row_index(AuthRow::Name),
            );
        }
        if rows.contains(&AuthRow::Value) {
            let mut token_spans = vec![Span::styled("       token  ", theme::muted())];
            token_spans.extend(secret_spans(&form.token, focused == AuthRow::Value));
            out.row(
                token_spans,
                focused == AuthRow::Value,
                row_index(AuthRow::Value),
            );
        }
        for note in account_notes(&form.account) {
            out.para(format!("      {note}"));
        }
    }

    out.blank();
    for line in form.effect() {
        out.para(*line);
    }

    let buttons = [
        (
            "Apply",
            focused == AuthRow::Apply,
            form.needs_recreate(),
            row_index(AuthRow::Apply),
        ),
        (
            "Cancel",
            focused == AuthRow::Cancel,
            false,
            row_index(AuthRow::Cancel),
        ),
    ];
    let title = app
        .sandboxes
        .iter()
        .find(|s| s.name == form.sandbox)
        .map(|s| format!("Anthropic account — {}", s.label))
        .unwrap_or_else(|| "Anthropic account".to_string());
    framed(frame, area, &title, out, &looks(&buttons))
}

/// What the sandbox is on today, in the same words the details pane uses.
fn current_spans(current: &Auth, users: usize) -> Vec<Span<'static>> {
    let (text, note) = crate::create::auth_now(current, users);
    let style = match current {
        Auth::Login => Style::default(),
        Auth::Token { stored: false, .. } => theme::danger(),
        Auth::Token { .. } => Style::default().fg(theme::RUNNING),
    };
    vec![
        Span::styled(format!(" {text}"), style.add_modifier(Modifier::BOLD)),
        Span::styled(format!("  — {note}"), theme::muted()),
    ]
}

/// The shared-skills row: a tick box in the columns of the Options above it, plus the mode, when
/// there is one, in the gloss column in the accent colour.
fn skills_spans(mode: SkillsMode, width: usize) -> Vec<Span<'static>> {
    // No "off": the empty box says it.
    let gloss = crate::create::skills_gloss(mode);
    let value = match mode {
        SkillsMode::Off => "",
        _ => crate::create::skills_mode_name(mode),
    };
    const MODE: usize = 12;
    let value = match value.is_empty() {
        true => String::new(),
        false => format!("{value:<MODE$}"),
    };
    let used = 5 + 14 + value.chars().count();
    vec![
        Span::styled(
            if mode == SkillsMode::Off {
                " [ ] "
            } else {
                " [x] "
            },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{:<14}", "share skills"),
            match mode {
                SkillsMode::Off => Style::default(),
                _ => Style::default().bold(),
            },
        ),
        Span::styled(value, Style::default().fg(theme::ACCENT)),
        Span::styled(
            super::clip(gloss, width.saturating_sub(used)),
            theme::muted(),
        ),
    ]
}

/// A field on the create wizard's Workspace step: its label, muted, in the column the option rows'
/// labels use, so every label and every value on the step lines up.
fn field_row_spans(label: &str, value: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(format!("     {label:<14}"), theme::muted())];
    spans.extend(value);
    spans
}

fn option_spans(checked: bool, label: &str, gloss: &str, width: usize) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            if checked { " [x] " } else { " [ ] " },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{label:<14}"),
            match checked {
                true => Style::default().bold(),
                false => Style::default(),
            },
        ),
        Span::styled(
            super::clip(gloss, width.saturating_sub(5 + 14)),
            theme::muted(),
        ),
    ]
}

/// What a host-cache addon's row says beside its name: what it found on this host, or why it
/// would mount nothing.
fn host_cache_text(addon: &crate::kits::Addon, found: &model::HostDetect) -> String {
    format!("{} · {}", addon.short(), found.reason(&addon.kit.name))
}

/// A host-cache addon's row: what it found on this host beside its name, or why it would mount
/// nothing (greyed out, no box to tick). `marker` is the screen's create-time-only caveat.
fn host_cache_spans(
    addon: &crate::kits::Addon,
    found: &model::HostDetect,
    checked: bool,
    marker: Option<Marker<'_>>,
    column: usize,
    width: usize,
) -> Vec<Span<'static>> {
    let name = addon.kit.name.as_str();
    let text = host_cache_text(addon, found);
    if found.detected() {
        return addon_spans(name, &text, checked, marker, None, column, width);
    }
    vec![
        Span::styled(" [-] ", theme::muted()),
        Span::styled(format!("{name:<14}"), theme::muted()),
        Span::styled(
            super::clip(&text, width.saturating_sub(5 + 14)),
            theme::muted(),
        ),
    ]
}

/// The kits editor's marker for a kit `sbx kit add` cannot add, and the gloss on its heading that
/// says what the marker costs.
const CREATE_TIME: &str = "create-time only";
const KITS_GLOSS: &str = "adding a create-time only kit rebuilds the sandbox";
/// Beside the kits editor's `agent` line: the one thing about a sandbox the editor cannot change.
const AGENT_FIXED: &str = "fixed — create a new sandbox to change it";
/// The kits editor's *Host caches* gloss: the wizard's, less the greying the editor does not do.
const HOST_CACHES_GLOSS: &str = "read-only, from this host — adding one rebuilds the sandbox";

/// What a kit row (or an MCP server row) says after its display text.
#[derive(Debug, Clone, Copy)]
enum Marker<'m> {
    /// A caveat in brackets, in the warning colour: an MCP server that is not ready yet.
    Warn(&'m str),
    /// The create wizard's `only now` and the kits editor's `create-time only`, muted: most kits
    /// carry one, and a warning on every row would drown out the warnings that matter.
    Quiet(&'m str),
}

impl Marker<'_> {
    fn width(self) -> usize {
        match self {
            Marker::Warn(text) => text.chars().count() + 4,
            Marker::Quiet(text) => text.chars().count() + 2,
        }
    }

    fn span(self) -> Span<'static> {
        match self {
            Marker::Warn(text) => Span::styled(format!("  ({text})"), theme::warn()),
            Marker::Quiet(text) => Span::styled(format!("  {text}"), theme::muted()),
        }
    }
}

/// One kit checkbox. `column`: the width the display text is padded to before the marker, so the
/// markers of a block line up (0 for none).
fn addon_spans(
    name: &str,
    display: &str,
    checked: bool,
    marker: Option<Marker<'_>>,
    change: Option<(&str, ratatui::style::Color)>,
    column: usize,
    width: usize,
) -> Vec<Span<'static>> {
    // The display name gives up its columns first: the marker and the change matter more.
    let tail =
        marker.map_or(0, Marker::width) + change.map_or(0, |(label, _)| label.chars().count() + 2);
    let room = width.saturating_sub(5 + 14 + tail);
    let display = match marker {
        Some(_) => format!(
            "{:<pad$}",
            super::clip(display, room),
            pad = column.min(room)
        ),
        None => super::clip(display, room),
    };
    let mut spans = vec![
        Span::styled(
            if checked { " [x] " } else { " [ ] " },
            Style::default().fg(theme::ACCENT),
        ),
        Span::styled(
            format!("{name:<14}"),
            Style::default()
                .fg(theme::kit_color(name))
                .add_modifier(if checked {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
        Span::styled(display, theme::muted()),
    ];
    if let Some(marker) = marker {
        spans.push(marker.span());
    }
    if let Some((label, color)) = change {
        spans.push(Span::styled(
            format!("  {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    spans
}

// --- LiteLLM connections (`S` → `n` / `↵`, `A`) ---------------------------------------------

/// One field of a connection: its label, the value (the key masked), and after the URL the host
/// it names. The row is cut to `width`, never wrapped, so the rows below keep their click boxes.
fn conn_field_spans(
    label: &str,
    input: &TextInput,
    focused: bool,
    secret: bool,
    hint: &'static str,
    width: u16,
) -> Vec<Span<'static>> {
    let room = (width as usize).saturating_sub(14);
    let shown = fit_input(input, focused, secret, room);
    let mut spans = vec![Span::styled(format!("{label:>12}  "), theme::muted())];
    spans.extend(field_spans(&shown, focused, false, hint));
    if label == "URL" && crate::litellm::check_url(input.value.trim()).is_ok() {
        let host = format!("  host {}", crate::litellm::host(input.value.trim()));
        let used = shown.value.chars().count() + usize::from(focused);
        if used + host.chars().count() <= room {
            spans.push(Span::styled(host, theme::muted()));
        }
    }
    spans
}

/// The part of a field that fits in `room` columns, masked first when it is a secret, with `…`
/// where it is cut. Unfocused, it shows the start; focused, a window that ends at the caret.
fn fit_input(input: &TextInput, focused: bool, secret: bool, room: usize) -> TextInput {
    let chars: Vec<char> = match secret {
        true => vec!['•'; input.value.chars().count()],
        false => input.value.chars().collect(),
    };
    let cursor = input.cursor.min(chars.len());
    // The caret takes a cell of its own once it sits past the last character.
    let needed = chars.len() + usize::from(focused && cursor == chars.len());
    if needed <= room || room < 2 {
        return TextInput {
            value: chars.into_iter().collect(),
            cursor,
        };
    }
    if !focused || cursor + 2 <= room {
        let value: String = chars[..room - 1].iter().chain(['…'].iter()).collect();
        return TextInput { value, cursor };
    }
    let start = cursor + 2 - room;
    let end = (cursor + 1).min(chars.len());
    let value: String = ['…'].iter().chain(chars[start..end].iter()).collect();
    TextInput {
        value,
        cursor: room - 1,
    }
}

/// The `S` dialog: a new LiteLLM connection, or an existing one's URL and key.
pub(super) fn render_connection(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Connection(form) = &app.screen else {
        return Hits::new();
    };
    let rows = form.rows();
    let focused = rows[form.focus.min(rows.len() - 1)];
    let row_index = |row: ConnRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    let editing = match &form.mode {
        ConnMode::Edit(name) => Some(name.as_str()),
        ConnMode::New => None,
    };

    let mut out = Lines::new(area);
    out.para(
        "The key goes to the host keychain as one global sbx secret; every sandbox on the \
         connection sees only its placeholder, which the proxy swaps for the key.",
    );
    out.blank();
    out.section("Connection");
    let fields = &form.fields;
    match editing {
        Some(name) => out.lines.push(Line::from(vec![
            Span::styled(format!("{:>12}  ", "name"), theme::muted()),
            Span::styled(
                name.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled("  — a new name is a new connection", theme::muted()),
        ])),
        None => out.row(
            conn_field_spans(
                "name",
                &fields.name,
                focused == ConnRow::Name,
                false,
                "(a–z, 0–9, -)",
                out.width,
            ),
            focused == ConnRow::Name,
            row_index(ConnRow::Name),
        ),
    }
    out.row(
        conn_field_spans(
            "URL",
            &fields.url,
            focused == ConnRow::Url,
            false,
            "(https://…)",
            out.width,
        ),
        focused == ConnRow::Url,
        row_index(ConnRow::Url),
    );
    out.row(
        conn_field_spans(
            "key",
            &fields.key,
            focused == ConnRow::Key,
            true,
            match editing {
                Some(_) => "(empty = keep the stored key)",
                None => "(paste it here)",
            },
            out.width,
        ),
        focused == ConnRow::Key,
        row_index(ConnRow::Key),
    );
    if editing.is_some() {
        let note = "Empty keeps the stored key; a new key reaches every sandbox on it at its next \
                    request.";
        out.explain_lines(focused == ConnRow::Key, "key", &[note.to_string()]);
    }
    if let Some(name) = editing {
        let users = app.litellm_users(name).len();
        out.blank();
        out.para(match users {
            0 => "No sandbox is on this connection.".to_string(),
            1 => "1 sandbox is on it: Save names what reaches it and what needs R.".to_string(),
            n => format!("{n} sandboxes are on it: Save names what reaches them and what needs R."),
        });
        out.para("A new host needs the key again — the stored one targets the old host only.");
    }

    let buttons = [
        (
            "Save",
            focused == ConnRow::Save,
            false,
            row_index(ConnRow::Save),
        ),
        (
            "Cancel",
            focused == ConnRow::Cancel,
            false,
            row_index(ConnRow::Cancel),
        ),
    ];
    let title = match editing {
        Some(name) => format!("LiteLLM connection — {name}"),
        None => "New LiteLLM connection".to_string(),
    };
    framed(frame, area, &title, out, &looks(&buttons))
}

/// How a folder's access reads: read-write, where the agent can delete, in the warning colour.
fn access_style(access: crate::folders::Access) -> Style {
    match access {
        crate::folders::Access::Read => theme::muted(),
        crate::folders::Access::Write => theme::warn(),
    }
}

/// `F`: the host folders every sandbox mounts, and the fields that add or change one.
pub(super) fn render_folders(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Folders(form) = &app.screen else {
        return Hits::new();
    };
    let rows = form.rows();
    let focused = form.focused();
    let row_index = |row: FoldersRow| rows.iter().position(|r| *r == row).unwrap_or(0);
    let help = |out: &mut Lines, row: FoldersRow| {
        let h = form.help(row);
        out.explain_lines(focused == row, h.title, &[h.text]);
    };

    let mut out = Lines::new(area);
    out.para(crate::app::FOLDERS_NOTE);
    if let Some(error) = &form.error {
        out.warn(error);
    }
    out.blank();
    out.section_with("Folders", "↵ edit below · d delete");
    if form.folders.is_empty() {
        out.note("     (none yet)");
    }
    let path_w = (form.folders.iter())
        .map(|f| model::tilde(&f.path).chars().count())
        .max()
        .unwrap_or(0);
    for (i, f) in form.folders.iter().enumerate() {
        let row = FoldersRow::Folder(i);
        out.row(
            vec![
                Span::raw("     "),
                Span::raw(format!("{:<path_w$}", model::tilde(&f.path))),
                Span::styled(
                    format!("  {:<10}", f.access.label()),
                    access_style(f.access),
                ),
                Span::raw(format!("  {}", f.purpose)),
            ],
            focused == row,
            row_index(row),
        );
        help(&mut out, row);
    }
    out.blank();
    out.section(match form.editing {
        Some(_) => "Change the folder",
        None => "Add a folder",
    });
    out.row(
        conn_field_spans(
            "folder",
            &form.path,
            focused == FoldersRow::Path,
            false,
            "(an absolute path or ~/…)",
            out.width,
        ),
        focused == FoldersRow::Path,
        row_index(FoldersRow::Path),
    );
    help(&mut out, FoldersRow::Path);
    out.row(
        vec![
            Span::raw(format!("{:>12}  ", "")),
            Span::raw("[ Browse… ]"),
            Span::styled("  pick the folder on the host", theme::muted()),
        ],
        focused == FoldersRow::Browse,
        row_index(FoldersRow::Browse),
    );
    help(&mut out, FoldersRow::Browse);
    let mark = |on: bool| if on { "◉" } else { "○" };
    out.row(
        vec![
            Span::styled(format!("{:>12}  ", "access"), theme::muted()),
            Span::raw(format!(
                "{} read-only   ",
                mark(form.access == crate::folders::Access::Read)
            )),
            Span::styled(
                format!(
                    "{} read-write",
                    mark(form.access == crate::folders::Access::Write)
                ),
                match form.access {
                    crate::folders::Access::Write => access_style(form.access),
                    crate::folders::Access::Read => Style::default(),
                },
            ),
        ],
        focused == FoldersRow::Access,
        row_index(FoldersRow::Access),
    );
    help(&mut out, FoldersRow::Access);
    out.row(
        conn_field_spans(
            "purpose",
            &form.purpose,
            focused == FoldersRow::Purpose,
            false,
            "(what the agent finds there)",
            out.width,
        ),
        focused == FoldersRow::Purpose,
        row_index(FoldersRow::Purpose),
    );
    help(&mut out, FoldersRow::Purpose);
    help(&mut out, FoldersRow::Save);
    help(&mut out, FoldersRow::Close);

    let buttons = [
        (
            "Save",
            focused == FoldersRow::Save,
            false,
            row_index(FoldersRow::Save),
        ),
        (
            "Close",
            focused == FoldersRow::Close,
            false,
            row_index(FoldersRow::Close),
        ),
    ];
    framed(frame, area, "Host folders", out, &looks(&buttons))
}

/// `A` on a LiteLLM sandbox: which connection it is on, and the one to move it to.
pub(super) fn render_conn_switch(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::ConnSwitch(form) = &app.screen else {
        return Hits::new();
    };
    let rows = form.rows();
    let focused = rows[form.focus.min(rows.len() - 1)];
    let row_index = |row: ConnSwitchRow| rows.iter().position(|r| *r == row).unwrap_or(0);

    let mut out = Lines::new(area);
    out.note(format!("sbx name: {}", form.sandbox));
    out.blank();
    out.section("Now");
    let now = match form.current.as_deref() {
        Some(cur) => match form.connections.iter().find(|c| c.name == cur) {
            Some(c) => format!(" LiteLLM {cur} · {}", crate::litellm::host(&c.url)),
            None => format!(" LiteLLM {cur} — no longer exists"),
        },
        None => " no LiteLLM connection".to_string(),
    };
    out.lines.push(Line::from(Span::styled(
        now,
        Style::default().add_modifier(Modifier::BOLD),
    )));

    out.blank();
    out.section("Move to");
    let mut spans = vec![Span::styled(
        format!(" {:<14}", "Connection"),
        theme::muted(),
    )];
    match form.picked() {
        Some(conn) => {
            spans.push(Span::styled(
                format!("‹ {} · {} ›", conn.name, crate::litellm::host(&conn.url)),
                Style::default().fg(theme::ACCENT),
            ));
            if form.current.as_deref() == Some(conn.name.as_str()) {
                spans.push(Span::styled(" (current)", theme::muted()));
            }
            if app.litellm_key_missing(&conn.name) {
                spans.push(Span::styled(" key missing", theme::warn()));
            }
        }
        None => spans.push(Span::styled(
            "‹ new connection… ›",
            Style::default().fg(theme::ACCENT),
        )),
    }
    out.row(
        spans,
        focused == ConnSwitchRow::Conn,
        row_index(ConnSwitchRow::Conn),
    );
    if form.picked().is_none() {
        let typed = &form.new_conn;
        for (row, label, input, hint) in [
            (ConnSwitchRow::Name, "name", &typed.name, "(a–z, 0–9, -)"),
            (ConnSwitchRow::Url, "URL", &typed.url, "(https://…)"),
            (ConnSwitchRow::Key, "key", &typed.key, "(paste it here)"),
        ] {
            let on = focused == row;
            out.row(
                conn_field_spans(label, input, on, row == ConnSwitchRow::Key, hint, out.width),
                on,
                row_index(row),
            );
        }
    }

    out.blank();
    let moving = form.moving();
    out.para(match moving {
        true => {
            "Apply rebuilds the sandbox on that connection: its kit is baked into the container."
        }
        false => "No changes.",
    });

    let buttons = [
        (
            "Apply",
            focused == ConnSwitchRow::Apply,
            moving,
            row_index(ConnSwitchRow::Apply),
        ),
        (
            "Cancel",
            focused == ConnSwitchRow::Cancel,
            false,
            row_index(ConnSwitchRow::Cancel),
        ),
    ];
    let title = app
        .sandboxes
        .iter()
        .find(|s| s.name == form.sandbox)
        .map(|s| format!("LiteLLM connection — {}", s.label))
        .unwrap_or_else(|| "LiteLLM connection".to_string());
    framed(frame, area, &title, out, &looks(&buttons))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn a_fixed_column_keeps_its_gap() {
        assert_eq!(column("abc", 8), "abc     ");
        assert_eq!(column("abcdef", 8), "abcdef  ", "it fits with its gap");
        assert_eq!(column("abcdefg", 8), "abcde…  ", "cut short of the gap");
        assert_eq!(column("abcdefghijkl", 8).chars().count(), 8);
    }

    #[test]
    fn a_row_too_wide_ends_in_an_ellipsis() {
        let row = || {
            vec![
                Span::raw("   claude-token-box   "),
                Span::styled("its sandbox no longer exists", theme::muted()),
            ]
        };
        assert_eq!(text(&fit_row(row(), 50)), text(&row()), "it fits");
        let cut = fit_row(row(), 40);
        assert_eq!(text(&cut), "   claude-token-box   its sandbox no lo…");
        assert_eq!(cut.iter().map(Span::width).sum::<usize>(), 40);
        assert_eq!(
            cut.last().unwrap().style.fg,
            theme::muted().fg,
            "the cut text's colour"
        );
    }

    #[test]
    fn a_long_fields_caret_stays_in_view() {
        let caret = Style::default().add_modifier(Modifier::REVERSED);
        let field = |before: &str, after: &str| {
            vec![
                Span::raw("   Token  "),
                Span::raw(before.to_string()),
                Span::styled(" ", caret),
                Span::raw(after.to_string()),
            ]
        };
        let dots = "•".repeat(100);
        let fitted = fit_row(field(&dots, ""), 40);
        assert_eq!(fitted.iter().map(Span::width).sum::<usize>(), 39);
        let shown = text(&fitted);
        assert!(shown.starts_with("   Token  …•"), "{shown}");
        let at = fitted
            .iter()
            .position(|s| s.style == caret)
            .expect("the caret");
        let caret_end: usize = fitted[..=at].iter().map(Span::width).sum();
        assert!(caret_end < 40, "the caret is on screen: {caret_end}");

        // In the middle of a long value: the start gives way, and the rest is cut after it.
        let fitted = fit_row(field(&"a".repeat(60), &"b".repeat(60)), 40);
        let shown = text(&fitted);
        assert_eq!(fitted.iter().map(Span::width).sum::<usize>(), 40);
        assert!(
            shown.starts_with("   Token  …a") && shown.ends_with('…'),
            "{shown}"
        );
        assert!(fitted.iter().any(|s| s.style == caret), "{shown}");
    }

    #[test]
    fn scrolls_only_once_the_focus_falls_below_the_viewport() {
        assert_eq!(scroll_for(Some((0, 0)), 30, 12), 0);
        assert_eq!(scroll_for(Some((9, 9)), 30, 12), 0);
        assert_eq!(scroll_for(Some((10, 10)), 30, 12), 1);
        // Everything fits → never scroll.
        assert_eq!(scroll_for(Some((9, 9)), 10, 20), 0);
    }

    #[test]
    fn scrolls_far_enough_to_show_the_note_under_the_focused_row() {
        // 10 rows of viewport. A row at 9 with three lines of note under it has to come up to 6,
        // or the note it just grew is drawn past the bottom border.
        assert_eq!(scroll_for(Some((9, 12)), 30, 12), 3);
        // …but never so far that the row itself leaves: a note longer than the viewport keeps its
        // row on the top line rather than scrolling past it.
        assert_eq!(scroll_for(Some((9, 40)), 60, 12), 9);
    }

    #[test]
    fn a_note_wraps_to_the_panel_and_keeps_its_indent() {
        let lines = wrap("     one two three four five", 20);
        assert_eq!(lines, vec!["     one two three", "     four five"]);
        // Short enough to leave alone, and too narrow to do anything sensible with.
        assert_eq!(wrap("     short", 20), vec!["     short"]);
        assert_eq!(wrap("     one two three", 9), vec!["     one two three"]);
    }

    #[test]
    fn the_kits_note_says_a_sandbox_from_another_checkout_is_rebuilt() {
        use crate::kits::Catalog;
        let catalog = Catalog::load().unwrap();
        let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
        sb.other_checkout = Some(std::path::PathBuf::from("/elsewhere"));
        let mut app = App::for_test(catalog, vec![sb]);
        app.open_kits("box");
        let jdk = app
            .catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "jdk25");
        if let Screen::Kits(form) = &mut app.screen {
            form.addons[jdk.unwrap()] = true;
        }
        let shown = crate::ui::tests_support::render_at(&mut app, 110, 44);
        assert!(
            shown.contains("Applying this rebuilds the sandbox: its kits come from another"),
            "{shown}"
        );
        assert!(!shown.contains("runs `sbx kit add`"), "{shown}");
    }

    #[test]
    fn the_kits_note_says_ram_volumes_lose_the_history() {
        use crate::kits::Catalog;
        let mut sb = model::sample("box", Some("claude-anthropic"), &[]);
        sb.tmpfs_volumes = true;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![sb]);
        app.open_kits("box");
        let jdk = app
            .catalog
            .addons
            .iter()
            .position(|a| a.kit.name == "jdk25");
        if let Screen::Kits(form) = &mut app.screen {
            form.addons[jdk.unwrap()] = true;
        }
        let shown = crate::ui::tests_support::render_at(&mut app, 110, 44);
        assert!(
            shown.contains("but its kit volumes are in RAM, so"),
            "{shown}"
        );
        assert!(!shown.contains("session history, the workspace"), "{shown}");
    }

    #[test]
    fn the_review_step_draws_cores_review() {
        use crate::create::CreateStep;
        use crate::kits::Catalog;
        let mut app = App::for_test(Catalog::load().unwrap(), Vec::new());
        app.cwd = std::path::PathBuf::from("/proj");
        app.open_create();
        let catalog = std::sync::Arc::clone(&app.catalog);
        let Screen::Create(form) = &mut app.screen else {
            panic!("the create form is open")
        };
        form.wizard_set_step(&catalog, CreateStep::Review);
        let Screen::Create(form) = &app.screen else {
            panic!("the create form is open")
        };
        let review = form.review(&app);
        let shown = crate::ui::tests_support::render_at(&mut app, 110, 44);
        let mut at = 0;
        let mut lines: Vec<(&str, String)> = review
            .answers
            .iter()
            .map(|a| (a.label, a.value.clone()))
            .collect();
        lines.push(("sbx name", review.sbx_name.clone()));
        lines.push(("Mounts", review.mounts[0].clone()));
        for (label, value) in lines {
            let line = format!(" {label:<15}{value}");
            let found = shown[at..].find(&line);
            assert!(found.is_some(), "{line:?} after byte {at} in\n{shown}");
            at += found.unwrap() + line.len();
        }
    }
}

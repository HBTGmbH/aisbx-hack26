//! The create wizard's screen: one step of the create form at a time, in a frame that does not
//! move. Top to bottom: the trail, the step's rows with the help strip right under them, the
//! ledger (on a tall terminal) and the buttons.

use super::*;
use crate::create::{
    connection_hint, connection_label, harness_hint, provider_harnesses, ConnChoice, CreateStep,
    Help, Review, WizardRow, LOGIN_TAG,
};

pub(super) fn render(frame: &mut Frame, app: &App, area: Rect) -> Hits {
    let Screen::Create(form) = &app.screen else {
        return Hits::new();
    };
    let catalog = &app.catalog;
    let step = form.wizard_step();
    let rows = form.wizard_rows(catalog);
    let focused = rows[form.wizard_focus().min(rows.len() - 1)];
    let index = |row: WizardRow| rows.iter().position(|r| *r == row).unwrap_or(usize::MAX);
    let field = match focused {
        WizardRow::Field(row) => Some(row),
        _ => None,
    };
    let field_index = |row: CreateRow| index(WizardRow::Field(row));

    let mut out = Lines::new(area);
    let (heading, gloss) = form.step_heading(step, catalog);
    step_heading(&mut out, heading, gloss);
    // The review's lines that lead back to the step owning their answer.
    let mut links: Vec<(usize, CreateStep)> = Vec::new();
    match step {
        CreateStep::Provider => provider_step(&mut out, app, form, focused, &index),
        CreateStep::Connection => litellm_row(&mut out, app, form, field, &field_index),
        CreateStep::Harness => harness_step(&mut out, app, form, focused, &index),
        CreateStep::Models => models_step(&mut out, app, form, focused, &index),
        CreateStep::Kits => kits_rows(&mut out, app, form, field, &field_index),
        CreateStep::Workspace => workspace_rows(&mut out, app, form, field, &field_index),
        CreateStep::Review => review_step(&mut out, app, form, &mut links),
    }
    mark_problems(&mut out, app, form, &rows);

    let title = format!(
        "New sandbox {} in {}",
        form.sbx_name(app),
        model::tilde(&app.cwd)
    );
    frame.render_widget(panel(&title, true), area);
    let mut hits = Hits::new();
    // The trail, at least one row, a blank line and the buttons, inside the borders.
    if area.height < 8 || area.width < 8 {
        return hits;
    }
    let x = area.x + 2;
    let width = area.width - 4;
    hits.extend(trail(frame, step, Rect::new(x, area.y + 1, width, 1)));

    // The rows are a panel of their own, so `Lines` scrolls and hit-tests them as on every form:
    // its borders are the blank line under the trail and the rule below (or a blank line).
    let strip = strip_height(area.height);
    let ledger = match step {
        CreateStep::Review => 0,
        _ => ledger_height(area.height),
    };
    let body = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height - 4 - strip - ledger,
    );
    let scroll = scroll_for(out.focus_block(), out.lines.len(), body.height);
    let shown = (out.lines.len() as u16)
        .saturating_sub(scroll)
        .min(body.height.saturating_sub(2));
    hits.extend(out.hits(body, scroll));
    for (line, to) in links {
        let Some(y) = (line as u16).checked_sub(scroll).map(|d| d + body.y + 1) else {
            continue;
        };
        if y < body.bottom().saturating_sub(1) {
            let rect = Rect::new(body.x + 1, y, body.width.saturating_sub(2), 1);
            hits.push((rect, Hit::WizardStep(to)));
        }
    }
    frame.render_widget(
        Paragraph::new(out.lines).scroll((scroll, 0)),
        Rect::new(x, body.y + 1, width, body.height - 2),
    );
    let bar_y = area.bottom() - 2;
    let rule_y = (body.y + 2 + shown).min(body.bottom() - 1);
    if strip > 0 {
        help_strip(
            frame,
            &form.help(app, focused),
            Rect::new(x, rule_y, width, strip + 1),
        );
    }
    if ledger > 0 {
        let strip_end = rule_y + strip + (strip > 0) as u16;
        let ledger_y = match strip_end < bar_y - ledger {
            true => bar_y - ledger - 1,
            false => bar_y - ledger,
        };
        let at = Rect::new(x, ledger_y, width, ledger);
        hits.extend(ledger_lines(frame, &form.review(app), step, at));
    }

    let mut bar = Lines::new(area);
    let buttons: Vec<(&str, bool, Look, usize)> = CreateForm::wizard_button_rows(step)
        .iter()
        .enumerate()
        .map(|(n, row)| {
            // Back stays on step 1, inert, so the buttons keep their places.
            let look = match (n, *row) {
                (0, _) => Look::Primary,
                (_, WizardRow::Back) if step == CreateStep::Provider => Look::Inert,
                _ => Look::Plain,
            };
            (button_label(*row), focused == *row, look, index(*row))
        })
        .collect();
    let drawn = button_line(&mut bar, &buttons);
    hits.extend(drawn.hits(Rect::new(area.x, bar_y - 1, area.width, 3), 0));
    frame.render_widget(Paragraph::new(bar.lines), Rect::new(x, bar_y, width, 1));
    hits
}

/// How many lines the help strip gets under the rule, from the panel's height alone, so the rows
/// above it stay put. Three from 80x24 up; none where the rows would have fewer than five lines.
fn strip_height(panel: u16) -> u16 {
    match panel {
        22.. => 3,
        16..=21 => 2,
        12..=15 => 1,
        _ => 0,
    }
}

/// How many lines the ledger gets: a rule and one line for each of steps 1 to 6, from a panel tall
/// enough that the rows of the fullest step still fit above it, and none below that.
fn ledger_height(panel: u16) -> u16 {
    match panel {
        30.. => 7,
        _ => 0,
    }
}

/// The width of the ledger's label column: "Connection" and a space.
const LEDGER_LABEL: usize = 12;

/// The ledger: a rule, then one line per step with its number and what it decided, each a click
/// target to its step. The step on screen is bold, and an answer still at its default is muted.
fn ledger_lines(frame: &mut Frame, review: &Review, step: CreateStep, area: Rect) -> Hits {
    let width = area.width as usize;
    let title = "so far";
    let rule = "─".repeat(width.saturating_sub(title.chars().count() + 3));
    let mut lines = vec![Line::from(vec![
        Span::styled("─ ", theme::border()),
        Span::styled(title, theme::muted()),
        Span::styled(format!(" {rule}"), theme::border()),
    ])];
    let mut hits = Hits::new();
    for s in CreateStep::ALL {
        let mut answers = review.digest(s).into_iter();
        let Some(first) = answers.next() else {
            continue;
        };
        let look = |default: bool| match (default, s == step) {
            (true, _) => theme::muted(),
            (false, true) => Style::default().bold(),
            (false, false) => Style::default(),
        };
        let mut spans = vec![
            Span::styled(format!(" {} ", s.number()), theme::key()),
            Span::styled(format!("{:<LEDGER_LABEL$}", first.label), theme::muted()),
            Span::styled(first.brief().to_string(), look(first.default)),
        ];
        for a in answers {
            spans.push(Span::styled(format!("   {} ", a.label), theme::muted()));
            spans.push(Span::styled(a.brief().to_string(), look(a.default)));
        }
        let y = area.y + lines.len() as u16;
        if y < area.bottom() {
            hits.push((Rect::new(area.x, y, area.width, 1), Hit::WizardStep(s)));
        }
        lines.push(Line::from(clip_spans(spans, width)));
    }
    frame.render_widget(Paragraph::new(lines), area);
    hits
}

/// `spans` cut to `width` columns where they are wider, the cut marked with `…` in the style of
/// the span it falls in.
fn clip_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= width || width == 0 {
        return spans;
    }
    let mut room = width - 1;
    let mut out = Vec::new();
    let mut style = Style::default();
    for span in spans {
        let len = span.content.chars().count();
        style = span.style;
        if len <= room {
            room -= len;
            out.push(span);
            continue;
        }
        let kept: String = span.content.chars().take(room).collect();
        out.push(Span::styled(kept, span.style));
        break;
    }
    out.push(Span::styled("…", style));
    out
}

/// The help strip: a rule naming the row under the cursor, then core's words on it (its problem,
/// its warning, what it does), wrapped to the panel and cut to the strip with an ellipsis.
fn help_strip(frame: &mut Frame, help: &Help, area: Rect) {
    let width = area.width as usize;
    let title = crate::ui::clip(&help.title, width.saturating_sub(6));
    let used = title.chars().count() + 3;
    let rule = Line::from(vec![
        Span::styled("─ ", theme::border()),
        Span::styled(title, Style::default().bold()),
        Span::styled(
            format!(" {}", "─".repeat(width.saturating_sub(used))),
            theme::border(),
        ),
    ]);
    let mut lines = vec![rule];
    let room = area.height.saturating_sub(1) as usize;
    let mut body: Vec<Line<'static>> = Vec::new();
    let parts = [
        help.problem
            .as_ref()
            .map(|p| (format!("✗ {p}"), theme::danger())),
        help.warn
            .as_ref()
            .map(|w| (format!("⚠ {w}"), theme::warn())),
        Some((help.text.clone(), Style::default())),
    ];
    for (text, style) in parts.into_iter().flatten() {
        for line in crate::ui::wrap_text(&text, width) {
            body.push(Line::from(Span::styled(line, style)));
        }
    }
    if body.len() > room {
        body.truncate(room);
        if let Some(last) = body.last_mut() {
            let text: String = last.spans.iter().map(|s| s.content.as_ref()).collect();
            let style = last.spans.first().map(|s| s.style).unwrap_or_default();
            let cut = crate::ui::clip(&format!("{text} …"), width);
            *last = Line::from(Span::styled(cut, style));
        }
    }
    lines.extend(body);
    frame.render_widget(Paragraph::new(lines), area);
}

/// Put a `!` in the first column of every row whose answer a create would refuse, once that is
/// worth saying ([`CreateForm::visible_problem`]) — unless the cursor's mark is there already.
fn mark_problems(out: &mut Lines<'_>, app: &App, form: &CreateForm, rows: &[WizardRow]) {
    for (line, index) in out.rows.clone() {
        let Some(WizardRow::Field(row)) = rows.get(index) else {
            continue;
        };
        if out.focus_line == Some(line) || form.visible_problem(*row, &app.catalog).is_none() {
            continue;
        }
        let Some(spans) = out.lines.get_mut(line).map(|l| &mut l.spans) else {
            continue;
        };
        let Some(first) = spans.first_mut() else {
            continue;
        };
        let Some(rest) = first.content.strip_prefix(' ') else {
            continue;
        };
        let rest = Span::styled(rest.to_string(), first.style);
        *first = Span::styled("!", theme::danger());
        spans.insert(1, rest);
    }
}

/// How the trail is written: in full where it fits, else with only the current step titled, else
/// the current step alone.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Trail {
    Full,
    Compact,
    Current,
}

/// The seven steps, numbered like the keys that jump there: the ones behind ticked, the current
/// one reversed, the ones ahead muted. Each is a click target.
fn trail(frame: &mut Frame, step: CreateStep, area: Rect) -> Hits {
    let fit = [Trail::Full, Trail::Compact]
        .into_iter()
        .find(|t| {
            trail_spans(step, *t)
                .iter()
                .map(|(s, _)| width(s))
                .sum::<u16>()
                <= area.width
        })
        .unwrap_or(Trail::Current);
    let mut spans = Vec::new();
    let mut hits = Hits::new();
    let mut at = 0u16;
    for (entry, to) in trail_spans(step, fit) {
        let len = width(&entry);
        if let Some(to) = to.filter(|_| at < area.width) {
            let rect = Rect::new(area.x + at, area.y, len.min(area.width - at), 1);
            hits.push((rect, Hit::WizardStep(to)));
        }
        spans.extend(entry);
        at = at.saturating_add(len);
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    hits
}

fn width(spans: &[Span<'_>]) -> u16 {
    Line::from(spans.to_vec()).width() as u16
}

/// The trail's pieces in `form`: each step's spans with the step a click on them goes to, and the
/// gaps between them, which go nowhere.
fn trail_spans(step: CreateStep, form: Trail) -> Vec<(Vec<Span<'static>>, Option<CreateStep>)> {
    let current = |s: CreateStep| {
        let text = match form {
            Trail::Current => format!(" {}/{} {} ", s.number(), CreateStep::ALL.len(), s.title()),
            Trail::Full | Trail::Compact => format!(" {} {} ", s.number(), s.title()),
        };
        vec![Span::styled(
            text,
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::REVERSED),
        )]
    };
    let mut out = Vec::new();
    for s in CreateStep::ALL {
        let entry = match (s.number().cmp(&step.number()), form) {
            (std::cmp::Ordering::Equal, _) => current(s),
            (_, Trail::Current) => continue,
            (std::cmp::Ordering::Less, Trail::Full) => vec![
                Span::styled("✓ ", Style::default().fg(theme::RUNNING)),
                Span::raw(format!("{} {}", s.number(), s.title())),
            ],
            (std::cmp::Ordering::Less, _) => vec![
                Span::styled("✓", Style::default().fg(theme::RUNNING)),
                Span::raw(s.number().to_string()),
            ],
            (std::cmp::Ordering::Greater, Trail::Full) => vec![Span::styled(
                format!("{} {}", s.number(), s.title()),
                theme::muted(),
            )],
            (std::cmp::Ordering::Greater, _) => {
                vec![Span::styled(s.number().to_string(), theme::muted())]
            }
        };
        if !out.is_empty() {
            out.push((vec![Span::raw("  ")], None));
        }
        out.push((entry, Some(s)));
    }
    out
}

/// The step's opening line, core's words: bold, its gloss beside it where both fit. Like a
/// section heading it is never wrapped; a gloss that does not fit is left off.
fn step_heading(out: &mut Lines<'_>, heading: String, gloss: &str) {
    let fits = heading.chars().count() + 3 + gloss.chars().count() <= out.width as usize;
    let mut spans = vec![Span::styled(
        crate::ui::clip(&heading, out.width as usize),
        Style::default().bold(),
    )];
    if !gloss.is_empty() && fits {
        spans.push(Span::styled(format!("   {gloss}"), theme::muted()));
    }
    out.lines.push(Line::from(spans));
}

fn button_label(row: WizardRow) -> &'static str {
    match row {
        WizardRow::Back => "Back",
        WizardRow::Next => "Next",
        WizardRow::CreateStart => "Create & start",
        WizardRow::CreateOnly => "Create only",
        WizardRow::Cancel => "Cancel",
        WizardRow::Provider(_)
        | WizardRow::Agent(_)
        | WizardRow::Connection(_)
        | WizardRow::Field(_) => "",
    }
}

/// A radio: a dot in `dot`'s colour, the name, an optional tag in the running colour and a muted
/// hint, cut to the row. Steps 1 and 2 draw every radio here, so their hints line up.
fn radio(
    chosen: bool,
    dot: ratatui::style::Color,
    label: &str,
    tag: Option<&str>,
    hint: &str,
    width: usize,
) -> Vec<Span<'static>> {
    const LABEL: usize = 18;
    let mut room = width.saturating_sub(3 + LABEL + 3);
    let mut spans = vec![
        Span::styled(
            format!(" {} ", if chosen { "◉" } else { "○" }),
            Style::default().fg(dot),
        ),
        Span::styled(
            format!("{label:<LABEL$}"),
            match chosen {
                true => Style::default().bold(),
                false => Style::default(),
            },
        ),
        Span::raw("   "),
    ];
    if let Some(tag) = tag {
        spans.push(Span::styled(
            format!("{tag} · "),
            Style::default().fg(theme::RUNNING).bold(),
        ));
        room = room.saturating_sub(tag.chars().count() + 3);
    }
    spans.push(Span::styled(crate::ui::clip(hint, room), theme::muted()));
    spans
}

/// Step 1: one radio per provider, with the harnesses it runs beside it.
fn provider_step(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: WizardRow,
    index: &dyn Fn(WizardRow) -> usize,
) {
    let catalog = &app.catalog;
    let picked = form.selected_provider_index(catalog);
    for (p, provider) in form.providers(catalog).into_iter().enumerate() {
        let harnesses = provider_harnesses(catalog, provider).join(" · ");
        let spans = radio(
            picked == Some(p),
            theme::ACCENT,
            provider.label(),
            None,
            &harnesses,
            out.width as usize,
        );
        let row = WizardRow::Provider(p);
        out.row(spans, focused == row, index(row));
    }
}

/// Step 2: one radio per agent of the provider, then what the chosen one needs — the account
/// radios and token rows, or the connection — what it still lacks on this host, and the model
/// pins where there are any.
fn harness_step(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: WizardRow,
    index: &dyn Fn(WizardRow) -> usize,
) {
    let catalog = &app.catalog;
    let field = match focused {
        WizardRow::Field(row) => Some(row),
        _ => None,
    };
    let field_index = |row: CreateRow| index(WizardRow::Field(row));
    let picked = form.selected_harness(catalog);
    for (h, harness) in form.harnesses(catalog).iter().enumerate() {
        let hint = (harness.backends.first())
            .map(|b| harness_hint(&catalog.backends[*b]))
            .unwrap_or_default();
        let spans = radio(
            picked == Some(h),
            theme::agent_color(&harness.agent),
            &harness.label,
            None,
            &hint,
            out.width as usize,
        );
        let row = WizardRow::Agent(h);
        out.row(spans, focused == row, index(row));
    }
    let agent = form.selected_agent(catalog).unwrap_or_default();
    let choices = form.connection_choices(catalog);
    if !choices.is_empty() {
        out.blank();
    }
    for choice in choices {
        let chosen = form.connection_selected(choice, catalog);
        let row = WizardRow::Connection(choice);
        let spans = radio(
            chosen,
            theme::agent_color(agent),
            connection_label(choice),
            (choice == ConnChoice::Login).then_some(LOGIN_TAG),
            &connection_hint(choice, form, catalog),
            out.width as usize,
        );
        out.row(spans, focused == row, index(row));
        if choice == ConnChoice::Token && chosen {
            token_rows(out, app, form, field, &field_index);
        }
    }
    // Drawn wherever the cursor is: a missing credential blocks the step's answer.
    let missing = catalog
        .backends
        .get(form.harness.backend)
        .map(|b| app.missing_secrets(b))
        .unwrap_or_default();
    if !missing.is_empty() {
        missing_secret_notes(out, &missing);
    }
}

/// Step 3, on a LiteLLM connection: the refresh row, the agent's slots with the offered models
/// under the open one, the config files the answers make, then every model of the connection,
/// ticked where the agent is offered it.
fn models_step(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    focused: WizardRow,
    index: &dyn Fn(WizardRow) -> usize,
) {
    let catalog = &app.catalog;
    let Some(agent) = form.models_agent(catalog) else {
        return;
    };
    let refresh = WizardRow::Field(CreateRow::ModelsRefresh);
    let spans = vec![
        Span::styled(" ⟳ ", Style::default().fg(theme::ACCENT)),
        Span::styled("Refresh the list", Style::default().bold()),
        Span::styled(
            format!("   {}", catalog.models_source.label()),
            theme::muted(),
        ),
    ];
    out.row(spans, focused == refresh, index(refresh));
    let offered = form.enabled_models(catalog);
    if form.models_offered(catalog) {
        out.blank();
        model_section(
            out,
            ModelList {
                title: "Starts on",
                gloss: "nothing picked leaves it to the agent",
                slots: Slot::of(agent),
                models: &catalog.models,
                offered: &offered,
                // The refresh row above says where the list came from.
                source: None,
            },
            &form.connection.models,
            form.connection.model_pick,
            focused,
            index,
            model_row,
            model_default_row,
            model_pick_row,
            false,
        );
    }
    config_files(out, app, focused, index);
    if !form.models_offered(catalog) {
        return;
    }
    out.blank();
    let heading = format!("Offered · {} of {}", offered.len(), catalog.models.len());
    out.lines.push(Line::from(Span::styled(
        heading,
        theme::muted().add_modifier(Modifier::BOLD),
    )));
    for (i, model) in catalog.models.iter().enumerate() {
        let on = !form.connection.disabled.contains(&model.id);
        let row = WizardRow::Field(CreateRow::ModelOn(i));
        let mut spans = vec![
            Span::styled(
                if on { " [x] " } else { " [ ] " },
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled(
                model.name.clone(),
                match on {
                    true => Style::default().bold(),
                    false => theme::muted(),
                },
            ),
        ];
        if model.name != model.id {
            spans.push(Span::styled(format!("   {}", model.id), theme::muted()));
        }
        out.row(spans, focused == row, index(row));
    }
}

/// The config files the connection writes for the agent: each a row that opens it in an
/// editor, marked where the user edited it.
fn config_files(
    out: &mut Lines<'_>,
    app: &App,
    focused: WizardRow,
    index: &dyn Fn(WizardRow) -> usize,
) {
    use aisbx_core::litellm::FileState;
    let rows: Vec<(usize, String, FileState)> = (0..)
        .map_while(|i| app.config_file_row(i).map(|(file, state)| (i, file, state)))
        .collect();
    if rows.is_empty() {
        return;
    }
    out.blank();
    out.section_with("Config files", "↵ edit · del reset");
    for (i, file, state) in rows {
        let row = WizardRow::Field(CreateRow::ConfigFile(i));
        let (mark, style) = match state {
            FileState::Generated => ("generated", theme::muted()),
            FileState::Edited => ("edited", Style::default().fg(theme::ACCENT)),
            FileState::Invalid(_) => ("edited, not valid JSON", theme::warn()),
        };
        let spans = vec![
            Span::raw("   "),
            Span::styled(file, Style::default().bold()),
            Span::styled(format!("   {mark}"), style),
        ];
        out.row(spans, focused == row, index(row));
    }
}

/// The model rows as the wizard's rows, for [`model_section`], which the kits editor shares.
fn model_row(slot: Slot) -> WizardRow {
    WizardRow::Field(CreateRow::Model(slot))
}

fn model_default_row(slot: Slot) -> WizardRow {
    WizardRow::Field(CreateRow::ModelDefault(slot))
}

fn model_pick_row(slot: Slot, i: usize) -> WizardRow {
    WizardRow::Field(CreateRow::ModelPick(slot, i))
}

/// One line of the review: the step's number (on its first line), the label and the answer. A
/// changed answer is bold and a default one muted, so changes stand out without colour too.
fn answer(out: &mut Lines<'_>, number: Option<usize>, label: &str, value: String, default: bool) {
    let number = number.map(|n| n.to_string()).unwrap_or_default();
    out.lines.push(Line::from(vec![
        Span::styled(format!(" {number:>1} "), theme::key()),
        Span::styled(format!("{label:<15}"), theme::muted()),
        match default {
            true => Span::styled(value, theme::muted()),
            false => Span::styled(value, Style::default().bold()),
        },
    ]));
}

/// Step 5: one line per answer, each a way back to its step, then what sbx will be told: the
/// name and the mounts. The lines are core's (`CreateForm::review`).
fn review_step(
    out: &mut Lines<'_>,
    app: &App,
    form: &CreateForm,
    links: &mut Vec<(usize, CreateStep)>,
) {
    let review = form.review(app);
    if review.policy_warning {
        out.warn(format!(
            " ⚠ {} — sbx creates no sandbox until then.",
            crate::sbx::POLICY_INIT_HINT
        ));
    }
    let mut last = None;
    for a in review.answers {
        links.push((out.lines.len(), a.step));
        let number = (last != Some(a.step)).then(|| a.step.number());
        last = Some(a.step);
        answer(out, number, a.label, a.value, a.default);
    }

    out.blank();
    let plain = |out: &mut Lines<'_>, label: &str, value: String| {
        out.lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(format!("{label:<15}"), theme::muted()),
            Span::raw(value),
        ]));
    };
    plain(out, "sbx name", review.sbx_name);
    let mut mounts = review.mounts.into_iter();
    plain(out, "Mounts", mounts.next().unwrap_or_default());
    for mount in mounts {
        plain(out, "", mount);
    }
    for note in review.notes {
        out.warn(format!("     ⚠ {note}"));
    }
}

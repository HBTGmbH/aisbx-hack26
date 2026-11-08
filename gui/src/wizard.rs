//! The create wizard: core's seven steps of the create form, one at a time, as a setup assistant
//! with a rail of the steps down the left. Core decides the rows, the cursor, the keys and the
//! answers; this module draws them and hands every click back as a `Hit`.

use aisbx_core::app::{AccountChoice, App, CreateForm, CreateRow, Hit, Screen, TextInput};
use aisbx_core::create::{
    self, connection_hint, connection_label, field_gloss, provider_blurb, ConnChoice, CreateStep,
    Help, Review, WizardRow,
};
use aisbx_core::models::{self, Slot};
use aisbx_core::{kits, litellm, model, startdir};
use eframe::egui::{self, FontId, RichText};

use crate::controls::{self, Badge, Input, Item, Mark, Tile, Tone};
use crate::form::{self, Look, Rows};
use crate::theme;

/// The widest the step's content grows, so its lines stay readable on a wide window.
const CONTENT: f32 = 760.0;

/// The narrowest a provider or harness tile gets before the tiles go one per line fewer.
const TILE: f32 = 230.0;

/// From this window width up, the rail shows each step's title and answers; narrower, only the
/// steps' marks.
const RAIL_FULL_FROM: f32 = 860.0;

/// The rail with only the steps' marks.
const RAIL_COMPACT: f32 = 60.0;

/// The widest an input box gets.
const FIELD: f32 = 460.0;

/// The full rail's width on a window `width` wide.
fn rail_width(width: f32) -> f32 {
    (width * 0.24).clamp(236.0, 300.0)
}

/// Draw the wizard and pass a click on a control, a button, a step or an answer to core.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let Screen::Create(form) = &app.screen else {
        return;
    };
    let view: &App = app;
    let catalog = &view.catalog;
    let step = form.wizard_step();
    let rows = form.wizard_rows(catalog);
    let focus = form.wizard_focus().min(rows.len() - 1);
    let mut list = Rows::new(ui, "wizard", focus);
    let mut jump = None;
    let mut action = None;
    let review = form.review(view);
    let window = ui.available_width();
    let full = window >= RAIL_FULL_FROM;

    // The rail first, so it runs the whole height beside the step and its buttons.
    let rail_frame = egui::Frame::side_top_panel(ui.style())
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(match full {
            true => egui::Margin::symmetric(18, 16),
            false => egui::Margin::symmetric(8, 16),
        });
    egui::Panel::left("wizard-rail")
        .resizable(false)
        .exact_size(if full {
            rail_width(window)
        } else {
            RAIL_COMPACT
        })
        .frame(rail_frame)
        .show(ui, |ui| rail(ui, view, form, &review, full, &mut jump));
    egui::Panel::bottom("wizard-buttons")
        .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(20, 4)))
        .show(ui, |ui| {
            let note = full.then(|| form.help(view, rows[focus]));
            button_bar(ui, &mut list, view, form, &rows, note.as_ref());
        });
    let page = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin {
        left: 28,
        right: 20,
        top: 8,
        bottom: 0,
    });
    egui::CentralPanel::default().frame(page).show(ui, |ui| {
        header(ui, view, form, &review, full);
        // Each step scrolls on its own: a step opens at its top, not where the last one was left.
        egui::ScrollArea::vertical()
            .id_salt(("wizard-scroll", step.number()))
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.set_max_width(CONTENT.min(ui.available_width()));
                ui.add_space(4.0);
                let mut at = Draw {
                    list: &mut list,
                    app: view,
                    form,
                    rows: &rows,
                    action: &mut action,
                };
                match step {
                    CreateStep::Provider => provider_step(ui, &mut at),
                    CreateStep::Connection => litellm_fields(ui, &mut at),
                    CreateStep::Harness => harness_step(ui, &mut at),
                    CreateStep::Models => models_step(ui, &mut at),
                    CreateStep::Kits => kits_step(ui, &mut at),
                    CreateStep::Workspace => workspace_step(ui, &mut at),
                    CreateStep::Review => review_step(ui, &mut at, &review, &mut jump),
                }
                ui.add_space(16.0);
            });
    });
    if let Some((i, code)) = action {
        app.click(Hit::FormFocus(i));
        app.on_key(aisbx_core::input::KeyEvent::from(code));
    } else if let Some(hit) = jump.or(list.clicked) {
        app.click(hit);
    }
}

/// What every step's drawing needs: the rows of the frame, the app, the form and its rows.
struct Draw<'a, 'f> {
    list: &'a mut Rows,
    app: &'f App,
    form: &'f CreateForm,
    rows: &'f [WizardRow],
    /// A row's own second button: the row by index, then the key core takes for it.
    action: &'a mut Option<(usize, aisbx_core::input::KeyCode)>,
}

impl Draw<'_, '_> {
    fn index(&self, row: WizardRow) -> Option<usize> {
        form::index_of(self.rows, row)
    }

    fn focused(&self, row: WizardRow) -> bool {
        self.list.focused(self.index(row))
    }

    fn help(&self, row: WizardRow) -> Help {
        self.form.help(self.app, row)
    }

    /// Hand a control's response to the rows, with core's help for the row as its tooltip.
    fn take(&mut self, response: egui::Response, row: WizardRow) {
        let index = self.index(row);
        let response = response.on_hover_text(tooltip(&self.help(row)));
        self.list.take(&response, index);
    }
}

fn dark(ui: &egui::Ui) -> bool {
    ui.visuals().dark_mode
}

/// Core's help for a row as a tooltip: what is wrong first, then what will bite, then what the
/// row does.
pub(crate) fn tooltip(help: &Help) -> String {
    let text = [
        help.problem.as_deref(),
        help.warn.as_deref(),
        Some(help.text.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|t| !t.is_empty())
    .collect::<Vec<_>>()
    .join("\n\n");
    theme::drawable(&text).into_owned()
}

/// `text` with its first letter in upper case, as the window writes a label core keeps lower
/// case for the TUI's rows.
pub(crate) fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The step's own heading over its form: core's line for what the step decides, and its gloss.
/// On a narrow window, whose rail has only the steps' marks, the title and the name come first.
fn header(ui: &mut egui::Ui, app: &App, form: &CreateForm, review: &Review, full: bool) {
    ui.add_space(14.0);
    if !full {
        ui.horizontal(|ui| {
            ui.label(theme::strong(RichText::new("New sandbox").size(18.0)));
            let folder = format!("in {}", model::tilde(&app.cwd));
            ui.label(RichText::new(folder).size(14.0).weak());
        });
        ui.label(RichText::new(&review.sbx_name).monospace().weak());
        ui.add_space(10.0);
    }
    let (heading, gloss) = form.step_heading(form.wizard_step(), &app.catalog);
    // The Kits step marks the kits that can go in later, not the ones that cannot.
    let gloss = match form.wizard_step() {
        CreateStep::Kits => create::ADD_LATER_GLOSS,
        _ => gloss,
    };
    ui.label(theme::strong(RichText::new(heading).size(21.0)));
    if !gloss.is_empty() {
        ui.add_space(2.0);
        ui.label(RichText::new(sentence(gloss)).size(13.5).weak());
    }
    ui.add_space(10.0);
}

/// The rail: on a full one the sandbox being built and its sbx name, then the seven steps with
/// their marks, titles and answers so far (`Review::digest`) and the create shortcut; on a compact
/// one only the marks.
/// A click on a step goes there, as core's trail rules allow (`Hit::WizardStep`).
fn rail(
    ui: &mut egui::Ui,
    app: &App,
    form: &CreateForm,
    review: &Review,
    full: bool,
    jump: &mut Option<Hit>,
) {
    if full {
        ui.label(theme::strong(RichText::new("New sandbox").size(20.0)));
        let folder = format!("in {}", model::tilde(&app.cwd));
        ui.label(RichText::new(folder).size(13.5).weak());
        ui.add_space(10.0);
        name_plate(ui, &review.sbx_name);
        ui.add_space(20.0);
    }
    let agent = form.selected_agent(&app.catalog).unwrap_or_default();
    for s in CreateStep::ALL {
        let response = rail_step(ui, review, s, form.wizard_step(), full, agent);
        if response.clicked() {
            *jump = Some(Hit::WizardStep(s));
        }
    }
    if full {
        ui.add_space(14.0);
        ui.label(RichText::new(create_anywhere(ui)).small().weak());
    }
}

/// What the create shortcut does, with its key as this platform writes it.
fn create_anywhere(ui: &egui::Ui) -> String {
    let save = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
        egui::Modifiers::COMMAND,
        egui::Key::S,
    ));
    format!("{save} creates and starts from any step")
}

/// The caption over the rail's [`name_plate`].
pub const SBX_NAME: &str = "sbx name";

/// The name sbx will give the sandbox, under a caption, drawn as a name to type: monospace on the
/// code fill. Not drawn as a field, because nothing here edits it.
fn name_plate(ui: &mut egui::Ui, name: &str) {
    ui.label(RichText::new(SBX_NAME).size(theme::SMALL).weak());
    ui.add_space(3.0);
    let frame = egui::Frame::new()
        .fill(ui.visuals().code_bg_color)
        .corner_radius(4)
        .inner_margin(egui::Margin::symmetric(7, 3));
    frame.show(ui, |ui| {
        ui.set_max_width(ui.available_width());
        let name = RichText::new(name).monospace().size(13.5).strong();
        ui.add(egui::Label::new(name).truncate().selectable(false))
            .on_hover_text("The name sbx gives the sandbox: sbx run, sbx exec and sbx rm take it");
    });
}

/// The size of a step's mark on the rail.
const MARK: f32 = 24.0;

/// One step on the rail. Its mark is ticked once the wizard is past it, filled in the accent while
/// it is on screen, and outlined ahead. On a full rail its title and answers stand beside the mark.
fn rail_step(
    ui: &mut egui::Ui,
    review: &Review,
    s: CreateStep,
    current: CreateStep,
    full: bool,
    agent: &str,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let weak = ui.visuals().weak_text_color();
    let text = ui.visuals().text_color();
    let strong = ui.visuals().strong_text_color();
    let state = s.number().cmp(&current.number());
    let digest = review.digest(s);
    let width = ui.available_width();
    let inner = width - MARK - 18.0;
    // The digest's lines, laid out first: the entry is as tall as they are.
    let lines: Vec<(std::sync::Arc<egui::Galley>, bool)> = match full {
        true => digest
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let value = match i {
                    0 => a.brief().to_string(),
                    _ => format!("{}: {}", a.label, a.brief()),
                };
                let color = if a.default { weak } else { text };
                let galley = rail_text(ui, &value, color, inner, 2);
                (galley, i == 0 && s == CreateStep::Harness)
            })
            .collect(),
        false => Vec::new(),
    };
    let title_height = MARK;
    let body: f32 = lines.iter().map(|(g, _)| g.size().y + 2.0).sum();
    let height = match full {
        true => 8.0 + title_height + body + 12.0,
        false => MARK + 26.0,
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::CLICK);
    let painter = ui.painter();
    let band = match full {
        true => rect,
        false => rect.shrink2(egui::vec2(2.0, 4.0)),
    };
    if state == std::cmp::Ordering::Equal {
        painter.rect_filled(
            band,
            controls::RADIUS,
            theme::accent(dark).gamma_multiply(0.12),
        );
    } else if response.hovered() {
        painter.rect_filled(
            band,
            controls::RADIUS,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    }
    let center = match full {
        true => egui::pos2(
            rect.left() + 8.0 + MARK / 2.0,
            rect.top() + 8.0 + MARK / 2.0,
        ),
        false => egui::pos2(rect.center().x, rect.top() + 13.0 + MARK / 2.0),
    };
    let reached = |n: usize| match n <= current.number() {
        true => theme::accent(dark),
        false => weak.gamma_multiply(0.6),
    };
    let x = center.x;
    if s != CreateStep::Provider {
        let stroke = egui::Stroke::new(2.0, reached(s.number()));
        painter.vline(x, rect.top()..=center.y - MARK / 2.0 - 3.0, stroke);
    }
    if s != CreateStep::Review {
        let stroke = egui::Stroke::new(2.0, reached(s.number() + 1));
        painter.vline(x, center.y + MARK / 2.0 + 3.0..=rect.bottom(), stroke);
    }
    let number = |color| {
        let font = FontId::proportional(13.0);
        let at = egui::Align2::CENTER_CENTER;
        painter.text(center, at, s.number().to_string(), font, color);
    };
    match state {
        std::cmp::Ordering::Less => {
            painter.circle_filled(center, MARK / 2.0, theme::running(dark));
            // A tick drawn rather than written: egui's fonts have no ✓.
            let mark = egui::Stroke::new(2.0, ui.visuals().panel_fill);
            let (a, b, c) = (
                center + egui::vec2(-5.0, 0.5),
                center + egui::vec2(-1.5, 4.0),
                center + egui::vec2(5.0, -3.5),
            );
            painter.line_segment([a, b], mark);
            painter.line_segment([b, c], mark);
        }
        std::cmp::Ordering::Equal => {
            painter.circle_filled(center, MARK / 2.0, theme::accent(dark));
            number(theme::on_accent(dark));
        }
        std::cmp::Ordering::Greater => {
            let fill = ui.visuals().extreme_bg_color;
            painter.circle_filled(center, MARK / 2.0, fill);
            painter.circle_stroke(center, MARK / 2.0 - 1.0, egui::Stroke::new(1.5, weak));
            number(text);
        }
    }
    if full {
        let title_color = match state {
            std::cmp::Ordering::Equal => strong,
            std::cmp::Ordering::Less => text,
            std::cmp::Ordering::Greater => weak,
        };
        let left = center.x + MARK / 2.0 + 10.0;
        let font = FontId::proportional(15.0);
        let title = painter.layout_no_wrap(s.title().to_string(), font, title_color);
        let y = center.y - title.size().y / 2.0;
        painter.galley(egui::pos2(left, y), title, title_color);
        let mut y = rect.top() + 8.0 + title_height + 2.0;
        for (galley, dot) in lines {
            let mut at = egui::pos2(left, y);
            if dot {
                let r = 4.0;
                let c = egui::pos2(left + r, y + galley.size().y / 2.0);
                painter.circle_filled(c, r, theme::agent_color(agent, dark));
                at.x += 2.0 * r + 6.0;
            }
            let size = galley.size().y;
            painter.galley(at, galley, text);
            y += size + 2.0;
        }
    }
    let answers: Vec<String> = digest.iter().map(|a| a.brief().to_string()).collect();
    let label = format!("Step {}: {}. {}", s.number(), s.title(), answers.join(", "));
    let on = state == std::cmp::Ordering::Equal;
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::Tab, true, on, &label));
    let tip = match (full, answers.is_empty()) {
        (true, _) => format!("Go to step {}: {}", s.number(), s.title()),
        (false, true) => format!("Step {}: {}", s.number(), s.title()),
        (false, false) => format!("Step {}: {}\n{}", s.number(), s.title(), answers.join("\n")),
    };
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(tip)
}

/// A line of the rail's digest laid out `width` wide, cut to `rows` lines with `…`.
fn rail_text(
    ui: &egui::Ui,
    text: &str,
    color: egui::Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_string(),
        egui::TextFormat::simple(FontId::proportional(13.0), color),
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(1.0),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// How many lines [`focus_note`] takes, so the bar keeps its height whatever the note says.
const NOTE_LINES: usize = 2;

/// The narrowest room the bar gives [`focus_note`]; narrower, it is left out.
const NOTE_MIN: f32 = 220.0;

/// Core's help for the row its cursor is on, in the bar between the way out and the way on: what
/// is wrong, what will bite and what the row does, in the weak colour, cut to [`NOTE_LINES`] with
/// `…` (the tooltip has it all). It reads without a pointer, as the keys move the cursor.
fn focus_note(ui: &mut egui::Ui, help: &Help, width: f32) {
    let font = FontId::proportional(theme::SMALL);
    let weak = ui.visuals().weak_text_color();
    let body = tooltip(help).replace("\n\n", " ");
    let mut job = theme::code_job(ui, &body, font.clone(), weak);
    job.wrap = egui::text::TextWrapping {
        max_width: width.min(form::PARA_W),
        max_rows: NOTE_LINES,
        ..Default::default()
    };
    let galley = ui.painter().layout_job(job);
    let line = ui.fonts_mut(|f| f.row_height(&font));
    let size = egui::vec2(width, line * NOTE_LINES as f32);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let top = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(rect.left(), top), galley, weak);
    response.on_hover_text(tooltip(help));
}

/// The step's buttons the way a desktop window has them: the way out at the left edge, the way
/// on at the right, its main action rightmost and filled in the accent. Each says its key. Between
/// them the cursor's [`focus_note`] where it is given, else the create shortcut.
fn button_bar(
    ui: &mut egui::Ui,
    list: &mut Rows,
    app: &App,
    form: &CreateForm,
    rows: &[WizardRow],
    note: Option<&Help>,
) {
    let step = form.wizard_step();
    let save = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
        egui::Modifiers::COMMAND,
        egui::Key::S,
    ));
    let key = |row: WizardRow| match row {
        WizardRow::Next => "PgDn".to_string(),
        WizardRow::Back => "Esc".to_string(),
        WizardRow::CreateStart => save.clone(),
        _ => String::new(),
    };
    let mut button = |ui: &mut egui::Ui, row: WizardRow, look: Look| {
        let index = form::index_of(rows, row);
        let response = list.button(ui, button_label(row), index, look);
        let help = form.help(app, row);
        let tip = match key(row) {
            k if k.is_empty() => help.text,
            k => format!("{}\n\nKey: {k}", help.text),
        };
        response.on_hover_text(tip);
    };
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        button(ui, WizardRow::Cancel, Look::Plain);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (n, row) in CreateForm::wizard_button_rows(step).iter().enumerate() {
                let look = match (n, *row) {
                    (_, WizardRow::Cancel) => continue,
                    (0, _) => Look::Primary,
                    // Back stays in its place on step 1, where there is nothing to go back to.
                    (_, WizardRow::Back) if step == CreateStep::Provider => Look::Inert,
                    _ => Look::Plain,
                };
                button(ui, *row, look);
            }
            ui.add_space(14.0);
            let room = ui.available_width() - 14.0;
            match note {
                Some(help) if room >= NOTE_MIN => focus_note(ui, help, room),
                Some(_) => {}
                None => {
                    ui.label(RichText::new(create_anywhere(ui)).small().weak());
                }
            }
        });
    });
    ui.add_space(8.0);
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
        | WizardRow::Field(_)
        | WizardRow::Edit(_) => "",
    }
}

/// Tiles as many to a line as fit, each as tall as the tallest, so a line of them reads as one
/// row. A click on the `n`-th is `row(n)`.
fn tile_grid<'t>(
    ui: &mut egui::Ui,
    at: &mut Draw<'_, '_>,
    tiles: Vec<Tile<'t>>,
    row: impl Fn(usize) -> WizardRow,
) {
    const GAP: f32 = 12.0;
    let avail = ui.available_width();
    let columns = (((avail + GAP) / (TILE + GAP)).floor() as usize).clamp(1, 3);
    let width = (avail - GAP * (columns - 1) as f32) / columns as f32;
    let height = (tiles.iter())
        .map(|t| controls::tile_height(ui, t, width))
        .fold(0.0, f32::max);
    let mut tiles = tiles.into_iter().enumerate().peekable();
    while tiles.peek().is_some() {
        let line: Vec<(usize, Tile<'_>)> = tiles.by_ref().take(columns).collect();
        let mut drawn = Vec::new();
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for (n, tile) in line {
                drawn.push((n, controls::tile(ui, tile, width, height)));
            }
        });
        for (n, response) in drawn {
            at.take(response, row(n));
        }
        ui.add_space(GAP);
    }
}

/// Step 1: one tile per provider, saying what it serves.
fn provider_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let dark = dark(ui);
    let picked = at.form.selected_provider_index(catalog);
    let providers = at.form.providers(catalog);
    let blurbs: Vec<String> = (providers.iter())
        .map(|p| sentence(provider_blurb(*p)))
        .collect();
    let tiles = (providers.iter().enumerate())
        .map(|(p, provider)| Tile {
            title: provider.label(),
            blurb: &blurbs[p],
            detail: "",
            badges: Vec::new(),
            color: theme::accent(dark),
            selected: picked == Some(p),
            focused: at.focused(WizardRow::Provider(p)),
        })
        .collect();
    tile_grid(ui, at, tiles, WizardRow::Provider);
}

/// Step 2: one tile per agent of the provider; under them what the chosen one still needs — the
/// account and its token, or a connection — then what this host lacks for it and the model pins.
fn harness_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let dark = dark(ui);
    let picked = at.form.selected_harness(catalog);
    let harnesses = at.form.harnesses(catalog);
    let blurbs: Vec<String> = (harnesses.iter())
        .map(|h| sentence(&at.form.harness_gloss(catalog, h)))
        .collect();
    let tiles = (harnesses.iter().enumerate())
        .map(|(h, harness)| Tile {
            title: &harness.label,
            blurb: &blurbs[h],
            detail: "",
            badges: Vec::new(),
            color: theme::agent_color(&harness.agent, dark),
            selected: picked == Some(h),
            focused: at.focused(WizardRow::Agent(h)),
        })
        .collect();
    tile_grid(ui, at, tiles, WizardRow::Agent);

    let agent = at.form.selected_agent(catalog).unwrap_or_default();
    let width = ui.available_width();
    let choices = at.form.connection_choices(catalog);
    if !choices.is_empty() {
        ui.label(theme::strong(RichText::new("Account").size(13.5)));
        ui.add_space(2.0);
    }
    for choice in choices {
        let row = WizardRow::Connection(choice);
        let chosen = at.form.connection_selected(choice, catalog);
        let mut badges = Vec::new();
        let tag = sentence(create::LOGIN_TAG);
        if choice == ConnChoice::Login {
            badges.push(Badge {
                text: &tag,
                color: theme::running(dark),
            });
        }
        let hint = sentence(&connection_hint(choice, at.form, catalog));
        let tile = Tile {
            title: &sentence(connection_label(choice)),
            blurb: &hint,
            detail: "",
            badges,
            color: theme::agent_color(agent, dark),
            selected: chosen,
            focused: at.focused(row),
        };
        let response = controls::tile(ui, tile, width, 0.0);
        at.take(response, row);
        if chosen && choice == ConnChoice::Token {
            nested(ui, |ui| token_fields(ui, at));
        }
        ui.add_space(8.0);
    }
    let missing = catalog
        .backends
        .get(at.form.harness.backend)
        .map(|b| at.app.missing_secrets(b))
        .unwrap_or_default();
    if !missing.is_empty() {
        let commands: Vec<String> = missing.iter().map(|s| kits::secret_set_hint(s)).collect();
        controls::banner(
            ui,
            Tone::Warn,
            &format!(
                "{} is missing on this computer. Store it with:\n{}",
                missing.join(" and "),
                commands.join("\n")
            ),
        );
    }
}

/// What a chosen tile still needs, set in under it.
fn nested(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.add_space(28.0);
        ui.vertical(|ui| add(ui));
    });
}

/// The LiteLLM connection a gateway backend is built on: one tile per connection on this host,
/// as the other steps offer their choices, then *new connection…* and, while that is picked,
/// its fields.
fn litellm_fields(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let row = WizardRow::Field(CreateRow::LitellmConn);
    let index = at.index(row);
    let form = at.form;
    let dark = dark(ui);
    let width = ui.available_width().min(FIELD + 120.0);
    let chosen = form.chosen(CreateRow::LitellmConn);
    let last = sentence("last used");
    let local = form.connection.new_conn.kind == Some(litellm::Kind::Local);
    let saved = form.connection.connections.len();
    for (n, name) in form.choices(CreateRow::LitellmConn).iter().enumerate() {
        let conn = form.connection.connections.get(n);
        let found = (n.checked_sub(saved)).and_then(|k| form.found_offered().get(k));
        let selected = chosen == Some(n);
        let (title, blurb) = match (conn, found) {
            (Some(c), _) => (c.title(), c.url.clone()),
            (None, Some(f)) => (name.clone(), sentence(&f.summary())),
            (None, None) if local => (
                sentence(name),
                "Add an engine on this computer: its port, and its key if it has one".to_string(),
            ),
            (None, None) => (
                sentence(name),
                "Add a gateway: its address and key".to_string(),
            ),
        };
        let mut badges = Vec::new();
        if conn.is_some_and(|c| form.connection.last_litellm.as_deref() == Some(c.name.as_str())) {
            badges.push(Badge {
                text: &last,
                color: theme::running(dark),
            });
        }
        let tile = Tile {
            title: &title,
            blurb: &blurb,
            detail: "",
            badges,
            color: theme::accent(dark),
            selected,
            focused: selected && at.list.focused(index),
        };
        let response = controls::tile(ui, tile, width, 0.0);
        // The chosen tile is the row: in view while the cursor is on it. A click picks its
        // value, which `choose` says after `take`.
        if selected {
            at.list.take(&response, index);
        }
        if response.clicked() {
            at.list.choose(index, n);
        }
        ui.add_space(8.0);
    }
    match form.litellm_choice() {
        Some(conn) => {
            if at.app.litellm_key_missing(&conn.name) {
                ui.add_space(4.0);
                let note = create::missing_key_note(conn);
                let note = note.trim_start_matches('⚠').trim();
                controls::banner(ui, Tone::Warn, &sentence(note));
            }
        }
        None => {
            let typed = &form.connection.new_conn;
            let (name, url, key) = match local {
                true => ("llama-server", "8080", "only if the engine has one"),
                false => (
                    "my-gateway",
                    "https://litellm.example.com",
                    "paste the gateway's key",
                ),
            };
            let fields = [
                (CreateRow::LitellmName, "Name", &typed.name, name, false),
                (CreateRow::LitellmUrl, "URL", &typed.url, url, true),
                (CreateRow::LitellmKey, "Key", &typed.key, key, false),
            ];
            for (field, label, input, placeholder, mono) in fields {
                text_field(ui, at, field, label, input, placeholder, mono);
            }
        }
    }
}

/// The setup tokens this host holds, *new token…* and, while that is picked, its name and value.
fn token_fields(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let dark = dark(ui);
    let form = at.form;
    let width = ui.available_width().min(FIELD + 120.0);
    for (i, token) in form.connection.tokens.iter().enumerate() {
        let row = WizardRow::Field(CreateRow::TokenPick(i));
        let users = create::token_users(at.app.token_users(token));
        let item = Item {
            mark: Mark::Radio,
            title: token,
            detail: &sentence(&users),
            badges: Vec::new(),
            color: theme::accent(dark),
            checked: form.connection.account == AccountChoice::Named(token.clone()),
            enabled: true,
            focused: at.focused(row),
        };
        let response = controls::check_item(ui, item, width);
        at.take(response, row);
    }
    let row = WizardRow::Field(CreateRow::TokenNew);
    let item = Item {
        mark: Mark::Radio,
        title: "New token…",
        detail: "",
        badges: Vec::new(),
        color: theme::accent(dark),
        checked: form.connection.account == AccountChoice::New,
        enabled: true,
        focused: at.focused(row),
    };
    let response = controls::check_item(ui, item, width);
    at.take(response, row);
    if form.connection.account == AccountChoice::New {
        let c = &form.connection;
        text_field(
            ui,
            at,
            CreateRow::TokenName,
            "Name",
            &c.token_name,
            "personal",
            false,
        );
        text_field(
            ui,
            at,
            CreateRow::Token,
            "Token",
            &c.token,
            "paste the setup token",
            false,
        );
    }
}

/// One of the form's text rows as a labelled input box: the label above, under it what is wrong
/// with the answer or what goes in it.
fn text_field(
    ui: &mut egui::Ui,
    at: &mut Draw<'_, '_>,
    field: CreateRow,
    label: &str,
    input: &TextInput,
    placeholder: &str,
    monospace: bool,
) {
    let row = WizardRow::Field(field);
    let problem = at.form.visible_problem(field, &at.app.catalog);
    let width = ui.available_width().min(FIELD);
    let focused = at.focused(row);
    // The generated label is a suggestion that the first character typed replaces (`field_key`).
    let suggested = field == CreateRow::Name && !at.form.workspace.name_touched;
    let gloss = match suggested {
        true => sentence(create::GENERATED_GLOSS),
        false => sentence(at.form.field_gloss_of(&at.app.catalog, field)),
    };
    let response = controls::labelled(ui, label, &gloss, problem.as_deref(), |ui| {
        let input = Input {
            label,
            input,
            masked: field.masked(),
            placeholder,
            focused,
            problem: problem.is_some(),
            monospace,
            suggested,
        };
        controls::input_box(ui, input, width)
    });
    at.take(response, row);
}

/// Step 4, on a LiteLLM connection: the refresh button, then two folds of optional answers: the
/// agent's slots each as a select with the offered models under the open one, and every model of
/// the connection, ticked where the agent is offered it; then the config files the answers make.
fn models_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let Some(agent) = at.form.models_agent(catalog) else {
        return;
    };
    let refresh = WizardRow::Field(CreateRow::ModelsRefresh);
    let response = ui
        .horizontal(|ui| {
            let button = ui.button("Refresh the list");
            ui.add_space(6.0);
            ui.label(RichText::new(sentence(&catalog.models_source.label())).weak());
            button
        })
        .inner;
    if at.focused(refresh) {
        controls::focus_ring(ui, response.rect);
    }
    at.take(response, refresh);
    if at.form.models_offered(catalog) {
        let open = at.form.wizard.models_open;
        fold(ui, at, CreateRow::ModelsFold, "Pick models", open);
        if open {
            nested(ui, |ui| {
                slots(ui, at, agent);
                offered(ui, at);
            });
        }
    }
    if at.app.config_file_row(0).is_some() {
        let open = at.form.wizard.files_open;
        fold(ui, at, CreateRow::FilesFold, "Config files", open);
        if open {
            nested(ui, |ui| config_files(ui, at));
        }
    }
}

/// A fold of optional answers ([`controls::fold`]), handed to the rows like any control.
fn fold(ui: &mut egui::Ui, at: &mut Draw<'_, '_>, row: CreateRow, title: &str, open: bool) {
    let summary = at.form.fold_summary(at.app, row);
    let gloss = sentence(field_gloss(row));
    let fold = controls::Fold {
        title,
        summary: summary.as_deref(),
        gloss: &gloss,
        open,
        focused: at.focused(WizardRow::Field(row)),
    };
    let width = ui.available_width().min(FIELD + 120.0);
    let response = controls::fold(ui, fold, width);
    at.take(response, WizardRow::Field(row));
}

/// The config files the connection writes for the agent: each with *Open*, and *Reset* where
/// the user edited it.
fn config_files(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    use aisbx_core::litellm::FileState;
    let rows: Vec<(usize, String, FileState)> = (0..)
        .map_while(|i| {
            at.app
                .config_file_row(i)
                .map(|(file, state)| (i, file, state))
        })
        .collect();
    if rows.is_empty() {
        return;
    }
    let dark = dark(ui);
    for (i, file, state) in rows {
        let row = WizardRow::Field(CreateRow::ConfigFile(i));
        let mut reset = false;
        let response = ui
            .horizontal(|ui| {
                ui.label(RichText::new(&file).monospace());
                let (text, color) = match state {
                    FileState::Generated => ("Generated", ui.visuals().weak_text_color()),
                    FileState::Edited => ("Edited", ui.visuals().text_color()),
                    FileState::Invalid(_) => ("Edited, not valid JSON", theme::warn(dark)),
                };
                ui.label(RichText::new(text).color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if state != FileState::Generated {
                        reset = ui.button("Reset").clicked();
                    }
                    ui.button("Open")
                })
                .inner
            })
            .inner;
        if at.focused(row) {
            controls::focus_ring(ui, response.rect);
        }
        if let (true, Some(index)) = (reset, at.index(row)) {
            *at.action = Some((index, aisbx_core::input::KeyCode::Delete));
        }
        at.take(response, row);
    }
}

/// The agent's slots, each a select with the offered models under the open one.
fn slots(ui: &mut egui::Ui, at: &mut Draw<'_, '_>, agent: litellm::Agent) {
    let catalog = &at.app.catalog;
    let dark = dark(ui);
    controls::section(ui, "Starts on", "The agent picks where nothing is picked.");
    let width = ui.available_width().min(FIELD);
    let offered = at.form.enabled_models(catalog);
    for slot in Slot::of(agent).iter().copied() {
        let row = WizardRow::Field(CreateRow::Model(slot));
        let open = at.form.connection.model_pick == Some(slot);
        let pinned = at.form.connection.models.get(slot);
        let shown = match pinned {
            Some(id) => models::display_name(&catalog.models, id).unwrap_or_else(|| id.to_string()),
            None => sentence(slot.unset()),
        };
        let response = ui
            .horizontal(|ui| {
                let (column, _) =
                    ui.allocate_exact_size(egui::vec2(120.0, 30.0), egui::Sense::hover());
                let text = ui.visuals().strong_text_color();
                ui.painter().text(
                    column.left_center(),
                    egui::Align2::LEFT_CENTER,
                    sentence(slot.label()),
                    FontId::proportional(14.0),
                    text,
                );
                let chevron = if open { "▾" } else { "▸" };
                select(
                    ui,
                    &shown,
                    chevron,
                    pinned.is_none(),
                    at.focused(row),
                    width - 128.0,
                )
            })
            .inner;
        at.take(response, row);
        if !open {
            continue;
        }
        nested(ui, |ui| {
            let list_width = ui.available_width().min(FIELD);
            let default = WizardRow::Field(CreateRow::ModelDefault(slot));
            let leave = sentence(slot.unset());
            let item = Item {
                mark: Mark::Radio,
                title: &leave,
                detail: "",
                badges: Vec::new(),
                color: theme::accent(dark),
                checked: pinned.is_none(),
                enabled: true,
                focused: at.focused(default),
            };
            let response = controls::check_item(ui, item, list_width);
            at.take(response, default);
            for i in offered.iter().copied() {
                let model = &catalog.models[i];
                let row = WizardRow::Field(CreateRow::ModelPick(slot, i));
                let item = Item {
                    mark: Mark::Radio,
                    title: &model.name,
                    detail: if model.name == model.id {
                        ""
                    } else {
                        &model.id
                    },
                    badges: Vec::new(),
                    color: theme::accent(dark),
                    checked: pinned == Some(model.id.as_str()),
                    enabled: true,
                    focused: at.focused(row),
                };
                let response = controls::check_item(ui, item, list_width);
                at.take(response, row);
            }
        });
    }
}

/// Every model of the connection, ticked where the agent is offered it.
fn offered(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let dark = dark(ui);
    let offered = at.form.enabled_models(catalog);
    controls::section(
        ui,
        &format!("Offered · {} of {}", offered.len(), catalog.models.len()),
        "Untick a model to hide it from the agent, in every sandbox on this connection.",
    );
    const GAP: f32 = 12.0;
    let avail = ui.available_width();
    let columns = if avail >= 700.0 { 2 } else { 1 };
    let width = (avail - GAP * (columns - 1) as f32) / columns as f32;
    let all: Vec<usize> = (0..catalog.models.len()).collect();
    for chunk in all.chunks(columns) {
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for &i in chunk {
                let model = &catalog.models[i];
                let row = WizardRow::Field(CreateRow::ModelOn(i));
                let item = Item {
                    mark: Mark::Check,
                    title: &model.name,
                    detail: if model.name == model.id {
                        ""
                    } else {
                        &model.id
                    },
                    badges: Vec::new(),
                    color: theme::accent(dark),
                    checked: !at.form.connection.disabled.contains(&model.id),
                    enabled: true,
                    focused: at.focused(row),
                };
                let response = controls::check_item(ui, item, width);
                at.take(response, row);
            }
        });
        ui.add_space(4.0);
    }
}

/// A select-like box: what is chosen and a chevron; muted while it is the default.
fn select(
    ui: &mut egui::Ui,
    shown: &str,
    chevron: &str,
    default: bool,
    focused: bool,
    width: f32,
) -> egui::Response {
    let visuals = ui.visuals();
    let weak = visuals.weak_text_color();
    let color = match default {
        true => weak,
        false => visuals.text_color(),
    };
    let (border, field, hover) = (
        visuals.widgets.noninteractive.bg_stroke.color,
        visuals.extreme_bg_color,
        visuals.widgets.hovered.bg_stroke.color,
    );
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width.max(120.0), 30.0), egui::Sense::CLICK);
    let stroke = match response.hovered() {
        true => egui::Stroke::new(1.0, hover),
        false => egui::Stroke::new(1.0, border),
    };
    ui.painter().rect(
        rect,
        controls::RADIUS,
        field,
        stroke,
        egui::StrokeKind::Inside,
    );
    let font = FontId::proportional(14.0);
    let galley = ui
        .painter()
        .layout_no_wrap(shown.to_string(), font.clone(), color);
    let y = rect.center().y - galley.size().y / 2.0;
    ui.painter()
        .galley(egui::pos2(rect.left() + 10.0, y), galley, color);
    ui.painter().text(
        egui::pos2(rect.right() - 12.0, rect.center().y),
        egui::Align2::CENTER_CENTER,
        chevron,
        font,
        weak,
    );
    if focused {
        controls::focus_ring(ui, rect);
    }
    let label = shown.to_string();
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, &label));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The Kits step's two groups, which the kits editor shows too: the kits that mount host
/// folders, then the rest.
pub const HOST_CACHES: &str = "Host caches";
pub const ADDITIONAL_KITS: &str = "Additional kits";

/// Step 3: the addon kits as items that can be ticked, the host caches first, two to a line where
/// the window is wide enough; then what a ticked kit will bite with.
fn kits_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let form = at.form;
    let dark = dark(ui);
    let order = form.addon_order(catalog);
    let host = |i: usize| form.kits.host.get(i).is_some_and(Option::is_some);
    let (hosts, others): (Vec<usize>, Vec<usize>) = order.into_iter().partition(|i| host(*i));
    let add_later = sentence(create::ADD_LATER);
    for (title, gloss, kits) in [
        (HOST_CACHES, "Read-only, from this computer.", hosts),
        (ADDITIONAL_KITS, "", others),
    ] {
        if kits.is_empty() {
            continue;
        }
        controls::section(ui, title, gloss);
        const GAP: f32 = 12.0;
        let avail = ui.available_width();
        let columns = if avail >= 700.0 { 2 } else { 1 };
        let width = (avail - GAP * (columns - 1) as f32) / columns as f32;
        for chunk in kits.chunks(columns) {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = GAP;
                for &i in chunk {
                    let addon = &catalog.addons[i];
                    let row = WizardRow::Field(CreateRow::Addon(i));
                    let found = form.kits.host.get(i).and_then(Option::as_ref);
                    let detail = match found {
                        Some(found) if found.detected() => {
                            format!("Found {}", found.found().join(", "))
                        }
                        Some(found) => format!("Not found: {}", found.reason(&addon.kit.name)),
                        None => addon.kit.first_sentence().to_string(),
                    };
                    let mut badges = Vec::new();
                    // Mark the exceptions: the step's gloss says that most kits go in only now.
                    if addon.create_time_reason().is_none() {
                        badges.push(Badge {
                            text: &add_later,
                            color: theme::running(dark),
                        });
                    }
                    let item = Item {
                        mark: Mark::Check,
                        title: &addon.kit.display_name,
                        detail: &detail,
                        badges,
                        color: theme::kit_color(&addon.kit.name, dark),
                        checked: form.kits.addons.get(i).copied().unwrap_or(false),
                        enabled: !form.addon_unavailable(i),
                        focused: at.focused(row),
                    };
                    let response = controls::check_item(ui, item, width);
                    at.take(response, row);
                }
            });
            ui.add_space(4.0);
        }
    }
    let picked = form.selected_addons(catalog);
    if picked.iter().any(|a| a == "web") {
        ui.add_space(8.0);
        controls::banner(
            ui,
            Tone::Warn,
            "Web lets this sandbox reach every host on the internet.",
        );
    }
    if picked.iter().any(|a| a == "headroom") {
        let agent = form
            .effective_backend(catalog)
            .map(|b| b.agent.as_str())
            .unwrap_or("");
        ui.add_space(8.0);
        let warning = kits::headroom_warning(agent);
        let warning = warning.trim_start_matches('⚠').trim();
        controls::banner(ui, Tone::Warn, &sentence(warning));
    }
}

/// Step 6: the label, the start folder and the folders picked to mount, the options, and under a
/// fold the ports, the blocked hosts and the MCP servers.
fn workspace_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let catalog = &at.app.catalog;
    let form = at.form;
    let dark = dark(ui);
    let ws = &form.workspace;
    text_field(
        ui,
        at,
        CreateRow::Name,
        "Label",
        &ws.name,
        "made from your answers",
        false,
    );
    if form.start_dir_offered(catalog) {
        let row = WizardRow::Field(CreateRow::StartDir);
        let root = form.workspace_root(&at.app.cwd);
        let opens = startdir::describe(&root, ws.start_dir.as_deref());
        let focused = at.focused(row);
        let gloss = sentence(field_gloss(CreateRow::StartDir));
        let response = controls::labelled(ui, "Starts in", &gloss, None, |ui| {
            ui.horizontal(|ui| {
                let path = RichText::new(opens).monospace().size(13.5);
                ui.add(egui::Label::new(path).selectable(false));
                ui.add_space(8.0);
                let response = ui.add(egui::Button::new("Change…").frame_when_inactive(true));
                if focused {
                    controls::focus_ring(ui, response.rect);
                }
                response
            })
            .inner
        });
        at.take(response, row);
    }
    folders(ui, at);

    controls::section(ui, "Options", "");
    let width = ui.available_width().min(FIELD + 120.0);
    let row = WizardRow::Field(CreateRow::Clone);
    let item = Item {
        mark: Mark::Check,
        title: "Clone mode",
        detail: &sentence(field_gloss(CreateRow::Clone)),
        badges: Vec::new(),
        color: theme::accent(dark),
        checked: ws.clone,
        enabled: true,
        focused: at.focused(row),
    };
    let response = controls::check_item(ui, item, width);
    at.take(response, row);
    if form.git_guard_offered() {
        let row = WizardRow::Field(CreateRow::GitGuard);
        let item = Item {
            mark: Mark::Check,
            title: "Git folder read-only",
            detail: &sentence(field_gloss(CreateRow::GitGuard)),
            badges: Vec::new(),
            color: theme::accent(dark),
            checked: ws.git_guard,
            enabled: true,
            focused: at.focused(row),
        };
        let response = controls::check_item(ui, item, width);
        at.take(response, row);
    }
    if form.skills_offered(catalog) {
        let row = WizardRow::Field(CreateRow::Skills);
        let index = at.index(row);
        let options: Vec<String> = form
            .choices(CreateRow::Skills)
            .iter()
            .map(|o| sentence(o))
            .collect();
        let gloss = sentence(create::skills_gloss(ws.skills));
        let focused = at.focused(row);
        let mut picked = None;
        let response = controls::labelled(ui, "Share skills", &gloss, None, |ui| {
            let (response, clicked) = controls::segmented(
                ui,
                "skills",
                &options,
                form.chosen(CreateRow::Skills),
                focused,
            );
            picked = clicked;
            response
        });
        at.take(response, row);
        if let Some(value) = picked {
            at.list.choose(index, value);
        }
    }

    let open = form.wizard.advanced_open;
    fold(ui, at, CreateRow::AdvancedFold, "More options", open);
    if !open {
        return;
    }
    nested(ui, |ui| advanced(ui, at));
}

/// The folders picked to mount, each with its access and *Remove*, then *Mount a folder…*.
fn folders(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let form = at.form;
    let picked = &form.workspace.extra_folders;
    let label = "Also mounts";
    if !picked.is_empty() {
        ui.add_space(6.0);
        ui.label(theme::strong(RichText::new(label).size(13.5)));
    }
    for (i, spec) in picked.iter().enumerate() {
        let row = WizardRow::Field(CreateRow::Folder(i));
        let index = at.index(row);
        let host = model::tilde(std::path::Path::new(aisbx_core::folders::spec_host(spec).0));
        let options: Vec<String> = form
            .choices(CreateRow::Folder(i))
            .iter()
            .map(|o| sentence(o))
            .collect();
        let focused = at.focused(row);
        let problem = form.visible_problem(CreateRow::Folder(i), &at.app.catalog);
        let mut chosen = None;
        let mut remove = false;
        let response = ui
            .horizontal(|ui| {
                let path = RichText::new(&host).monospace().size(13.5);
                ui.add(egui::Label::new(path).truncate().selectable(false));
                ui.add_space(8.0);
                let (response, clicked) = controls::segmented(
                    ui,
                    &format!("folder-{i}"),
                    &options,
                    form.chosen(CreateRow::Folder(i)),
                    focused,
                );
                chosen = clicked;
                ui.add_space(8.0);
                remove = ui.button("Remove").clicked();
                response
            })
            .inner;
        if let Some(problem) = problem {
            let red = theme::danger(dark(ui));
            ui.label(RichText::new(problem).size(12.5).color(red));
        }
        at.take(response, row);
        if let Some(value) = chosen {
            at.list.choose(index, value);
        }
        if let (true, Some(index)) = (remove, index) {
            *at.action = Some((index, aisbx_core::input::KeyCode::Delete));
        }
    }
    let row = WizardRow::Field(CreateRow::FolderAdd);
    let focused = at.focused(row);
    let gloss = sentence(field_gloss(CreateRow::FolderAdd));
    let response = controls::labelled(ui, "Mount more folders", &gloss, None, |ui| {
        let response = ui.add(egui::Button::new("Mount a folder…").frame_when_inactive(true));
        if focused {
            controls::focus_ring(ui, response.rect);
        }
        response
    });
    at.take(response, row);
}

/// Under the Workspace step's fold: the ports, the blocked hosts and the MCP servers.
fn advanced(ui: &mut egui::Ui, at: &mut Draw<'_, '_>) {
    let form = at.form;
    let dark = dark(ui);
    let ws = &form.workspace;
    text_field(
        ui,
        at,
        CreateRow::Ports,
        "Ports",
        &ws.ports,
        "8080:3000",
        true,
    );
    text_field(
        ui,
        at,
        CreateRow::DenyNetwork,
        "Blocked hosts",
        &ws.deny_network,
        "api.example.com",
        true,
    );
    controls::section(ui, "MCP servers", "");
    let width = ui.available_width().min(FIELD + 120.0);
    let row = WizardRow::Field(CreateRow::McpNone);
    let item = Item {
        mark: Mark::Check,
        title: &sentence(create::NO_MCP),
        detail: &sentence(field_gloss(CreateRow::McpNone)),
        badges: Vec::new(),
        color: theme::accent(dark),
        checked: ws.no_mcp,
        enabled: true,
        focused: at.focused(row),
    };
    let response = controls::check_item(ui, item, width);
    at.take(response, row);
    for (i, server) in ws.mcp.iter().enumerate() {
        let row = WizardRow::Field(CreateRow::Mcp(i));
        let status = format!("Not ready: {}", server.status);
        let mut badges = Vec::new();
        if !server.ready() {
            badges.push(Badge {
                text: &status,
                color: theme::warn(dark),
            });
        }
        let item = Item {
            mark: Mark::Check,
            title: &server.name,
            detail: "",
            badges,
            color: theme::accent(dark),
            checked: ws.static_mcp.get(i).copied().unwrap_or(false),
            enabled: true,
            focused: at.focused(row),
        };
        let response = controls::check_item(ui, item, width);
        at.take(response, row);
    }
}

/// Step 7: the answers grouped by the step that owns them, each group with an Edit button back
/// to it, an answer still at its default muted; then the folders the sandbox sees.
fn review_step(ui: &mut egui::Ui, at: &mut Draw<'_, '_>, review: &Review, jump: &mut Option<Hit>) {
    let weak = ui.visuals().weak_text_color();
    if review.policy_warning {
        controls::banner(
            ui,
            Tone::Danger,
            &format!(
                "{} — sbx creates no sandbox until then.",
                sentence(aisbx_core::sbx::POLICY_INIT_HINT)
            ),
        );
        ui.add_space(8.0);
    }
    let steps: Vec<CreateStep> = CreateStep::ALL
        .into_iter()
        .filter(|s| review.answers.iter().any(|a| a.step == *s))
        .collect();
    for step in steps {
        ui.add_space(10.0);
        let row = WizardRow::Edit(step);
        let focused = at.focused(row);
        let group = egui::Frame::new()
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(controls::RADIUS)
            .inner_margin(egui::Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(theme::strong(RichText::new(step.title()).size(14.5)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let edit = ui
                            .add(egui::Button::new("Edit").frame_when_inactive(true))
                            .on_hover_text(format!("Go to step {}", step.number()));
                        if edit.clicked() {
                            *jump = Some(Hit::WizardStep(step));
                        }
                    });
                });
                ui.add_space(4.0);
                let answers: Vec<&create::Answer> =
                    review.answers.iter().filter(|a| a.step == step).collect();
                // One answer named like its step is the group's value; several are a table.
                let alone = answers.len() == 1 && answers[0].label == step.title();
                let value = |ui: &mut egui::Ui, a: &create::Answer, jump: &mut Option<Hit>| {
                    let text = match a.default {
                        true => RichText::new(&a.value).color(weak),
                        false => RichText::new(&a.value),
                    };
                    let response = ui
                        .add(
                            egui::Label::new(text)
                                .selectable(false)
                                .sense(egui::Sense::CLICK),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(format!(
                            "Change it on step {}: {}",
                            a.step.number(),
                            a.step.title()
                        ));
                    if response.clicked() {
                        *jump = Some(Hit::WizardStep(a.step));
                    }
                };
                if alone {
                    value(ui, answers[0], jump);
                    return;
                }
                egui::Grid::new(("wizard-review", step.number()))
                    .num_columns(2)
                    .spacing([24.0, 6.0])
                    .show(ui, |ui| {
                        for a in answers {
                            ui.label(RichText::new(a.label).color(weak));
                            value(ui, a, jump);
                            ui.end_row();
                        }
                    });
            });
        if focused {
            controls::focus_ring(ui, group.response.rect);
        }
        let index = at.index(row);
        at.list.take(&group.response, index);
    }
    ui.add_space(12.0);
    ui.label(theme::strong(RichText::new("Folders").size(14.5)));
    for folder in &review.mounts {
        ui.label(RichText::new(folder).monospace());
    }
    for note in &review.notes {
        ui.add_space(4.0);
        controls::banner(ui, Tone::Warn, note);
    }
}

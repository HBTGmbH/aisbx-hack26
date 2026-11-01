//! Core's modals as windows: the confirmation, the prompt, the job log and the help sheet. All
//! text comes from `app.modal`; every answer goes back through core's methods, which the TUI's
//! keys call too. Core's `tick` closes a job that succeeded.

use std::time::Duration;

use aisbx_core::app::{App, Modal, Prompt};
use aisbx_core::job::{Progress, Status, StepState, StepView};
use eframe::egui::{self, Color32, FontId, Key, Modifiers, Pos2, Rect, RichText, Stroke, Vec2};

use crate::{controls, help, theme};

/// Whether the prompt's field shows dots instead of characters: whenever core says the value is
/// a secret.
pub fn password_field(prompt: &Prompt) -> bool {
    prompt.masked()
}

/// One key for the open confirmation, in the order pressed: ← → Tab move the choice, ↵ and Space
/// answer the one selected then, `y` yes, `n` and Esc no. Returns whether the key was its.
pub fn confirm_key(app: &mut App, key: Key, modifiers: Modifiers) -> bool {
    let Some(Modal::Confirm(confirm)) = &mut app.modal else {
        return false;
    };
    if !modifiers.matches_logically(Modifiers::NONE) {
        return false;
    }
    match key {
        Key::ArrowLeft | Key::ArrowRight | Key::Tab => confirm.yes = !confirm.yes,
        Key::Enter | Key::Space => {
            let yes = confirm.yes;
            app.confirm_answer(yes);
        }
        Key::Y => app.confirm_answer(true),
        Key::N | Key::Escape => app.confirm_answer(false),
        _ => return false,
    }
    true
}

/// One key for the open help sheet: Esc, ↵, `?` and `q` close it, through core as its own keys
/// do. Every other key stays with the sheet's scroll area. Returns whether the key was its.
pub fn help_key(app: &mut App, key: Key, modifiers: Modifiers) -> bool {
    if !matches!(app.modal, Some(Modal::Help)) {
        return false;
    }
    let closes = match key {
        Key::Escape | Key::Enter | Key::Questionmark => true,
        Key::Q => !modifiers.ctrl && !modifiers.command,
        _ => false,
    };
    if closes {
        app.on_key(aisbx_core::input::KeyEvent::from(
            aisbx_core::input::KeyCode::Esc,
        ));
    }
    closes
}

/// Draw the open modal, if any, and pass the answer to core. `keys`: whether this frame's keys
/// are the modal's — not in the frame a key opened it. `covered`: another window is open over it,
/// so it is drawn disabled and takes neither the keyboard nor a click.
pub fn show(ctx: &egui::Context, app: &mut App, keys: bool, covered: bool) {
    let dark = ctx.global_style().visuals.dark_mode;
    match &mut app.modal {
        None => {}
        Some(Modal::Help) => help::show(ctx, app, covered),
        Some(Modal::Confirm(confirm)) => {
            // Its keys were answered by `confirm_key`; egui's own Tab focus move is undone.
            ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::None));
            let yes_now = confirm.yes;
            let answer = window(ctx, "confirm", covered, |ui| {
                ui.heading(&confirm.title);
                ui.add_space(6.0);
                for line in &confirm.body {
                    match line.is_empty() {
                        true => ui.add_space(6.0),
                        false => {
                            ui.add(egui::Label::new(theme::with_code(ui, line, |t| t)).wrap());
                        }
                    }
                }
                ui.add_space(14.0);
                let mut answer = None;
                // Right to left: the answer at the right edge, then Cancel, which is chosen first.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = theme::strong(RichText::new(&confirm.confirm_label));
                    let (fill, text) = match confirm.danger {
                        true => (theme::danger(dark), theme::on_danger(dark)),
                        false => (theme::accent(dark), theme::on_accent(dark)),
                    };
                    let ok = egui::Button::new(label.color(text))
                        .fill(fill)
                        .min_size(BUTTON);
                    let ok = ui.add(ok);
                    let cancel = ui.add(
                        egui::Button::new("Cancel")
                            .selected(!yes_now)
                            .min_size(BUTTON),
                    );
                    // The keyboard focus follows `yes`, so Space clicks the choice that is shown.
                    let chosen = if yes_now { &ok } else { &cancel };
                    if !covered && !chosen.has_focus() {
                        chosen.request_focus();
                    }
                    answer = (cancel.clicked().then_some(false)).or(ok.clicked().then_some(true));
                });
                answer
            });
            if let Some(yes) = answer {
                app.confirm_answer(yes);
            }
        }
        Some(Modal::Prompt(prompt)) => {
            let title = prompt.title();
            let password = password_field(prompt);
            let escape = keys && ctx.input(|i| i.key_pressed(Key::Escape));
            let answer = window(ctx, "prompt", covered, |ui| {
                ui.heading(title);
                ui.label(RichText::new(&prompt.subject).weak());
                ui.add_space(6.0);
                // The value lives in core's `Prompt.input` only; the field edits it in place.
                let mut value = prompt.input.value.clone();
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut value)
                        .password(password)
                        .desired_width(f32::INFINITY),
                );
                if value != prompt.input.value {
                    prompt.input.set(value);
                }
                let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                for line in &prompt.help {
                    ui.add(egui::Label::new(RichText::new(line).small().weak()).wrap());
                }
                ui.add_space(14.0);
                let mut answer = entered.then_some(true);
                // Right to left: the answer, named by what it does, then Cancel.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let label = theme::strong(RichText::new(answer_label(prompt.verb())))
                        .color(theme::on_accent(dark));
                    let ok = ui.add(
                        egui::Button::new(label)
                            .fill(theme::accent(dark))
                            .min_size(BUTTON),
                    );
                    let cancel = ui.add(egui::Button::new("Cancel").min_size(BUTTON));
                    answer = answer
                        .or(cancel.clicked().then_some(false))
                        .or(ok.clicked().then_some(true));
                    // The field has the keyboard unless Tab moved it onto these buttons.
                    let ours = [edit.id, cancel.id, ok.id];
                    if !covered && !ui.memory(|m| m.focused().is_some_and(|f| ours.contains(&f))) {
                        edit.request_focus();
                    }
                });
                answer
            });
            match answer.or(escape.then_some(false)) {
                Some(true) => app.prompt_submit(),
                Some(false) => app.prompt_cancel(),
                None => {}
            }
        }
        Some(Modal::Running(job, pending)) => {
            let finished = job.is_finished();
            let status = job.status();
            let fallback = (pending.fallback.as_ref())
                .filter(|_| finished)
                .map(|f| f.title.clone());
            let hint = pending.fallback_hint.clone();
            let steps = job.steps();
            let progress = job.progress();
            let elapsed = job.elapsed();
            let title = job.title.clone();
            let (mut close, mut run_fallback) = (false, false);
            window(ctx, "job", covered, |ui| {
                // As wide as the window gets: the bar and the steps fill it from the first frame.
                ui.set_min_width(JOB_W);
                ui.heading(&title);
                ui.add_space(4.0);
                let (fraction, text) = job_progress(&status, progress.as_ref());
                let (said, fill) = match &status {
                    Status::Failed(_) => (theme::danger(dark), theme::danger(dark)),
                    Status::Running | Status::Ok => {
                        (ui.visuals().weak_text_color(), theme::accent(dark))
                    }
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new(text).color(said));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(duration(elapsed)).weak());
                    });
                });
                // The track in the stripes' grey: on the field colour a thin bar is not seen.
                ui.scope(|ui| {
                    ui.visuals_mut().extreme_bg_color = ui.visuals().faint_bg_color;
                    let bar = egui::ProgressBar::new(fraction)
                        .desired_width(ui.available_width())
                        .desired_height(BAR_H)
                        .fill(fill);
                    ui.add(bar);
                });
                ui.add_space(10.0);
                // Keep `min_scrolled_height` at `max_height`: a modal's scroll area is sized from
                // the frame before and does not grow past its first size otherwise.
                let room = (ctx.content_rect().height() - JOB_CHROME).clamp(OUTPUT_H, STEPS_H);
                egui::ScrollArea::vertical()
                    .id_salt("aisbx-job-steps")
                    .max_height(room)
                    .min_scrolled_height(room)
                    .auto_shrink([false, true])
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let note = progress.as_ref().and_then(|p| p.note.as_deref());
                        step_list(ui, &title, &steps, note);
                    });
                if let Status::Failed(why) = &status {
                    ui.add_space(10.0);
                    let why = theme::with_code(ui, why, |t| t.color(theme::danger(dark)));
                    ui.add(egui::Label::new(why).wrap());
                    // The hint shows with or without a fallback to run.
                    if let Some(hint) = &hint {
                        ui.add(egui::Label::new(theme::with_code(ui, hint, |t| t.weak())).wrap());
                    }
                    ui.add_space(14.0);
                    // Right to left: the rebuild, red as it recreates the container, then Close.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(what) = &fallback {
                            let label = theme::strong(RichText::new(FALLBACK))
                                .color(theme::on_danger(dark));
                            run_fallback = ui
                                .add(
                                    egui::Button::new(label)
                                        .fill(theme::danger(dark))
                                        .min_size(BUTTON),
                                )
                                .on_hover_text(what)
                                .clicked();
                        }
                        close = ui
                            .add(egui::Button::new("Close").min_size(BUTTON))
                            .clicked();
                    });
                }
            });
            let escape = keys && ctx.input(|i| i.key_pressed(Key::Escape));
            if run_fallback {
                app.job_fallback();
            } else if close || escape {
                app.job_close();
            }
        }
    }
}

/// A job's steps, one row each: a mark, the step and its time. Under the running step its note and
/// output, under a failed one its output; a finished step's output opens on a click. At most
/// [`WAITING_SHOWN`] steps to come are named, so the running step's output stays in view.
fn step_list(ui: &mut egui::Ui, job: &str, steps: &[StepView], note: Option<&str>) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let total = steps.len();
    let waiting = steps
        .iter()
        .filter(|s| s.state == StepState::Waiting)
        .count();
    let mut shown_waiting = 0;
    for (i, step) in steps.iter().enumerate() {
        if step.state == StepState::Waiting {
            shown_waiting += 1;
            if shown_waiting > WAITING_SHOWN {
                continue;
            }
        }
        let id = egui::Id::unique(("aisbx-job-step", job, i));
        let open_by_default = matches!(step.state, StepState::Running | StepState::Failed);
        let open = ui
            .data(|d| d.get_temp::<bool>(id))
            .unwrap_or(open_by_default);
        let header = step_header(ui, step, i + 1, total, open);
        let open = match header.clicked() {
            true => {
                ui.data_mut(|d| d.insert_temp(id, !open));
                !open
            }
            false => open,
        };
        if open && step.state != StepState::Waiting {
            let note = note.filter(|_| step.state == StepState::Running);
            step_output(ui, step, i, note);
        }
    }
    let more = waiting.saturating_sub(WAITING_SHOWN);
    if more > 0 {
        let text = match more {
            1 => "and 1 more step".to_string(),
            n => format!("and {n} more steps"),
        };
        ui.horizontal(|ui| {
            ui.add_space(OUTPUT_X);
            ui.label(RichText::new(text).size(theme::SMALL).weak());
        });
        ui.add_space(4.0);
    }
}

/// One step's row: its mark, the step and its time, and — once it started — a triangle that
/// opens and closes its output, as a click anywhere on the row does.
fn step_header(
    ui: &mut egui::Ui,
    step: &StepView,
    number: usize,
    total: usize,
    open: bool,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let started = step.state != StepState::Waiting;
    let sense = match started {
        true => egui::Sense::click(),
        false => egui::Sense::hover(),
    };
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, STEP_H), sense);
    let state = match step.state {
        StepState::Waiting => "waiting",
        StepState::Running => "running",
        StepState::Done => "done",
        StepState::Failed => "failed",
    };
    let time = step.elapsed.map(duration);
    let label = format!(
        "Step {number} of {total}: {}, {state}{}",
        step.label,
        time.as_ref().map(|t| format!(", {t}")).unwrap_or_default()
    );
    response.widget_info(|| egui::WidgetInfo::selected(egui::Role::ListItem, true, open, &label));
    let response = match started {
        true => response.on_hover_cursor(egui::CursorIcon::PointingHand),
        false => response,
    };
    if response.hovered() && started {
        let band = rect.shrink2(Vec2::new(2.0, 1.0));
        ui.painter()
            .rect_filled(band, 4.0, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let weak = ui.visuals().weak_text_color();
    let center = Pos2::new(rect.left() + 8.0 + MARK / 2.0, rect.center().y);
    step_mark(ui, step.state, center);
    let mut right = rect.right() - 8.0;
    if started {
        let tip = Pos2::new(right - 4.0, rect.center().y);
        let triangle = match open {
            true => vec![
                tip + Vec2::new(-4.0, -2.5),
                tip + Vec2::new(4.0, -2.5),
                tip + Vec2::new(0.0, 3.5),
            ],
            false => vec![
                tip + Vec2::new(-2.5, -4.0),
                tip + Vec2::new(-2.5, 4.0),
                tip + Vec2::new(3.5, 0.0),
            ],
        };
        ui.painter()
            .add(egui::Shape::convex_polygon(triangle, weak, Stroke::NONE));
        right -= 18.0;
    }
    if let Some(time) = time {
        let galley =
            ui.painter()
                .layout_no_wrap(time, FontId::proportional(theme::SMALL + 0.5), weak);
        let at = Pos2::new(
            right - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        );
        right = at.x - 12.0;
        ui.painter().galley(at, galley, weak);
    }
    let color = match step.state {
        StepState::Waiting => weak,
        StepState::Running => ui.visuals().strong_text_color(),
        StepState::Done => ui.visuals().text_color(),
        StepState::Failed => theme::danger(dark),
    };
    let left = rect.left() + OUTPUT_X;
    let font = match step.state {
        StepState::Running => FontId::new(theme::BODY, theme::strong_family()),
        _ => FontId::proportional(theme::BODY),
    };
    let mut job = egui::text::LayoutJob::single_section(
        step.label.clone(),
        egui::TextFormat::simple(font, color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width((right - left).max(0.0));
    let galley = ui.painter().layout_job(job);
    let at = Pos2::new(left, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(at, galley, color);
    response
}

/// A step's mark: ticked in the running colour once done, a spinner while it runs, a cross in
/// the danger colour when it failed, an empty ring while it waits — the wizard rail's marks.
fn step_mark(ui: &egui::Ui, state: StepState, center: Pos2) {
    let dark = ui.visuals().dark_mode;
    let painter = ui.painter();
    let r = MARK / 2.0;
    match state {
        StepState::Done => {
            painter.circle_filled(center, r, theme::running(dark));
            controls::tick(painter, center, ui.visuals().panel_fill);
        }
        StepState::Running => {
            let rect = Rect::from_center_size(center, Vec2::splat(MARK + 2.0));
            egui::Spinner::new()
                .color(theme::accent(dark))
                .paint_at(ui, rect);
        }
        StepState::Failed => {
            painter.circle_filled(center, r, theme::danger(dark));
            let cross = Stroke::new(2.0, theme::on_danger(dark));
            let d = 3.5;
            painter.line_segment(
                [center + Vec2::new(-d, -d), center + Vec2::new(d, d)],
                cross,
            );
            painter.line_segment(
                [center + Vec2::new(-d, d), center + Vec2::new(d, -d)],
                cross,
            );
        }
        StepState::Waiting => {
            let ring = Stroke::new(1.5, ui.visuals().weak_text_color());
            painter.circle_stroke(center, r - 1.0, ring);
        }
    }
}

/// Under a step's row: the running step's note, then a sunken well with the command and what it
/// printed. The well scrolls on its own and keeps its newest line in view.
fn step_output(ui: &mut egui::Ui, step: &StepView, index: usize, note: Option<&str>) {
    let dark = ui.visuals().dark_mode;
    let weak = ui.visuals().weak_text_color();
    egui::Frame::new()
        .inner_margin(egui::Margin {
            left: OUTPUT_X as i8,
            right: 4,
            top: 0,
            bottom: 8,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if let Some(note) = note {
                let note = RichText::new(note).size(theme::SMALL).color(weak);
                ui.add(egui::Label::new(note).wrap());
                ui.add_space(6.0);
            }
            egui::Frame::new()
                .fill(ui.visuals().extreme_bg_color)
                .stroke(Stroke::new(1.0, theme::line(dark)))
                .corner_radius(6)
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt(("aisbx-job-output", index))
                        .max_height(OUTPUT_H)
                        .min_scrolled_height(OUTPUT_H)
                        .auto_shrink([false, true])
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            let line = |ui: &mut egui::Ui, text: &str, color: Color32| {
                                let text = RichText::new(theme::drawable(text))
                                    .font(FontId::monospace(LOG_SIZE))
                                    .color(color);
                                ui.add(egui::Label::new(text).wrap());
                            };
                            line(ui, &format!("$ {}", step.command), weak);
                            for printed in &step.output {
                                line(ui, printed, log_color(ui, printed, dark));
                            }
                        });
                });
        });
}

/// A line of a step's output in the look of its kind: sbx's phase headers (`── PREPARE IMAGE`)
/// bright, the commands it runs (`$ …`) weak, an error in the danger colour.
fn log_color(ui: &egui::Ui, line: &str, dark: bool) -> Color32 {
    if line.starts_with("── ") {
        ui.visuals().strong_text_color()
    } else if line.starts_with("$ ") {
        ui.visuals().weak_text_color()
    } else if line.starts_with("error:") {
        theme::danger(dark)
    } else {
        ui.visuals().text_color()
    }
}

/// A step's or a job's time, as the window writes it: tenths of a second under ten (`0.4 s`),
/// whole seconds under a minute (`42 s`), then minutes and seconds (`1 min 5 s`).
pub fn duration(elapsed: Duration) -> String {
    let secs = elapsed.as_secs();
    match secs {
        0..10 => format!("{:.1} s", (elapsed.as_millis() / 100) as f64 / 10.0),
        10..60 => format!("{secs} s"),
        _ => format!("{} min {} s", secs / 60, secs % 60),
    }
}

/// A dialog's buttons: an easy target, and one width for short labels.
const BUTTON: egui::Vec2 = egui::vec2(84.0, 30.0);

/// The job window's width at least — the most the modal windows take (`window`).
const JOB_W: f32 = 560.0;

/// The job's bar: a thin track under the line that says where the job is.
const BAR_H: f32 = 6.0;

/// The most the job's steps take before they scroll, and the most a step's output does.
const STEPS_H: f32 = 420.0;
const OUTPUT_H: f32 = 200.0;

/// What the job window needs around its steps: its title, the line and the bar over them, a
/// failure's reason and buttons under them, and its margins.
const JOB_CHROME: f32 = 280.0;

/// A step's row, the size of its mark, and where its text and output start.
const STEP_H: f32 = 30.0;
const MARK: f32 = 16.0;
const OUTPUT_X: f32 = 8.0 + MARK + 10.0;

/// The steps still to come that the list names before it counts the rest.
const WAITING_SHOWN: usize = 3;

/// The size of a step's output: a terminal's text, smaller than the window's.
const LOG_SIZE: f32 = 12.0;

/// The failed job's offer to rebuild instead, by the name the page gives a rebuild; its
/// tooltip is the fallback job's title.
pub const FALLBACK: &str = "Rebuild instead";

/// A prompt's answer button: core's verb, capitalised — *Rename*, *Publish*, *Store*.
pub fn answer_label(verb: &str) -> String {
    let mut chars = verb.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// How far a job's bar is filled, and what the line over it says: the step running now of how
/// many (`Step 2 of 3`); full once the job ended, saying how. The step's name is the list's.
pub fn job_progress(status: &Status, progress: Option<&Progress>) -> (f32, String) {
    match (status, progress) {
        (Status::Ok, _) => (1.0, "Done".into()),
        (Status::Failed(_), Some(p)) if p.total > 1 => {
            (1.0, format!("Failed at step {} of {}", p.index, p.total))
        }
        (Status::Failed(_), _) => (1.0, "Failed".into()),
        (Status::Running, Some(p)) if p.total > 1 => (
            (p.index - 1) as f32 / p.total as f32,
            format!("Step {} of {}", p.index, p.total),
        ),
        (Status::Running, Some(_)) => (0.0, "Running".into()),
        (Status::Running, None) => (0.0, "Starting…".into()),
    }
}

/// A modal window in the middle of the screen, with a dimmed backdrop.
fn window<T>(
    ctx: &egui::Context,
    id: &str,
    covered: bool,
    add: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    egui::Modal::new(egui::Id::unique(("aisbx-modal", id)))
        .show(ctx, |ui| {
            if covered {
                ui.disable();
            }
            ui.set_max_width(JOB_W);
            add(ui)
        })
        .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::app::{Ask, SecretTarget, TextInput};

    fn prompt(ask: Ask) -> Prompt {
        Prompt {
            ask,
            subject: "box".into(),
            input: TextInput::new(""),
            help: Vec::new(),
        }
    }

    #[test]
    fn a_duration_reads_in_tenths_seconds_then_minutes() {
        let ms = Duration::from_millis;
        assert_eq!(duration(ms(0)), "0.0 s");
        assert_eq!(duration(ms(420)), "0.4 s");
        assert_eq!(duration(ms(9_990)), "9.9 s");
        assert_eq!(duration(ms(10_000)), "10 s");
        assert_eq!(duration(ms(42_700)), "42 s");
        assert_eq!(duration(ms(65_000)), "1 min 5 s");
        assert_eq!(duration(ms(3_600_000)), "60 min 0 s");
    }

    #[test]
    fn the_line_over_the_bar_says_where_the_job_is() {
        let p = |index, total| Progress {
            index,
            total,
            label: "sbx create box".into(),
            note: None,
        };
        let failed = Status::Failed("x".into());
        assert_eq!(
            job_progress(&Status::Running, Some(&p(2, 4))),
            (0.25, "Step 2 of 4".into())
        );
        assert_eq!(
            job_progress(&Status::Running, Some(&p(1, 1))),
            (0.0, "Running".into())
        );
        assert_eq!(
            job_progress(&Status::Running, None),
            (0.0, "Starting…".into())
        );
        assert_eq!(
            job_progress(&failed, Some(&p(3, 4))),
            (1.0, "Failed at step 3 of 4".into())
        );
        assert_eq!(
            job_progress(&failed, Some(&p(1, 1))),
            (1.0, "Failed".into())
        );
        assert_eq!(
            job_progress(&Status::Ok, Some(&p(4, 4))),
            (1.0, "Done".into())
        );
    }

    #[test]
    fn a_secret_prompt_is_a_password_field() {
        assert!(!password_field(&prompt(Ask::Rename)));
        let secret = Ask::SetSecret(SecretTarget::Token("work".into()));
        assert!(password_field(&prompt(secret)));
    }
}

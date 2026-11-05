//! The sandbox list down the window's left: core's rows and sections (`App::roster`), one row per
//! sandbox, grouped by folder. Narrower than [`NARROW`], a row is a navigator of up to three
//! lines; wider, its three columns line up down the list.

use std::sync::Arc;

use aisbx_core::actions::Action;
use aisbx_core::app::{App, Hit};
use aisbx_core::input::KeyCode;
use aisbx_core::model::{CardMarker, CardNote, Sandbox};
use aisbx_core::roster::{self, FolderHead, Row};
use eframe::egui::{
    self, Color32, CursorIcon, FontId, Galley, Key, Pos2, Rect, Role, Sense, Stroke, Vec2,
    WidgetInfo,
};

use crate::{actions, theme};

/// Text sizes: a row's first line, its second line, a kit badge, a heading.
const FIRST: f32 = 14.0;
const SECOND: f32 = 12.5;
const BADGE: f32 = 11.5;
const HEADING: f32 = 16.0;
/// One line of a row, and the row's room above and below its lines.
const LINE_H: f32 = 21.0;
const PAD_Y: f32 = 6.0;
/// Room at a row's sides, the status dot's column, and the gap between two columns.
const PAD_X: f32 = 12.0;
const DOT_W: f32 = 20.0;
const GAP: f32 = 20.0;
/// Below this width the list uses the navigator layout: the middle column moves under the name.
pub(crate) const NARROW: f32 = 560.0;
/// A kit badge's padding, and the room between two badges.
const BADGE_PAD: Vec2 = Vec2::new(6.0, 1.5);
const BADGE_GAP: f32 = 5.0;
const RADIUS: f32 = 6.0;

/// The height of a sandbox's row of one line, of two and of three.
pub const ROW_H: [f32; 3] = [
    LINE_H + 2.0 * PAD_Y,
    2.0 * LINE_H + 2.0 * PAD_Y,
    3.0 * LINE_H + 2.0 * PAD_Y,
];

/// The keys that move the selection, through `App::on_key`.
pub const NAV_KEYS: [(Key, KeyCode); 8] = [
    (Key::ArrowUp, KeyCode::Up),
    (Key::ArrowDown, KeyCode::Down),
    (Key::ArrowLeft, KeyCode::Left),
    (Key::ArrowRight, KeyCode::Right),
    (Key::Home, KeyCode::Home),
    (Key::End, KeyCode::End),
    (Key::PageUp, KeyCode::PageUp),
    (Key::PageDown, KeyCode::PageDown),
];

/// The note as the row shows it, and the tooltip that names the fix.
pub fn note_text(note: CardNote) -> (String, &'static str) {
    match note {
        CardNote::Unresponsive => (
            "⚠ not responding".into(),
            "The sandbox has stopped responding. Stop it with s, then start it again.",
        ),
        CardNote::NoToken => (
            "⚠ no token".into(),
            "Its setup token is not stored, so it cannot sign in. Store one with A.",
        ),
        CardNote::Approvals(n) => (
            format!("⚑ {n} to approve"),
            "Requests are waiting for an answer. Answer them with y.",
        ),
        CardNote::KitsChanged(kind) => (format!("⟳ {}", kind.note()), "Rebuild it with R."),
    }
}

/// A marker's glyph, and its tooltip naming the condition.
fn marker_text(marker: CardMarker, waiting: usize) -> (&'static str, String) {
    match marker {
        CardMarker::Approvals => (
            "⚑",
            format!(
                "{waiting} request{} waiting for approval. Answer with y.",
                if waiting == 1 { "" } else { "s" }
            ),
        ),
        CardMarker::KitsChanged => ("⟳", "Rebuild it with R.".into()),
    }
}

/// Where a row's columns start (from the row's left edge) and how wide they are — the same for
/// every row, so the columns line up.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Columns {
    name: (f32, f32),
    runs: (f32, f32),
    state: (f32, f32),
    /// The navigator's layout: the state at the right edge after the name, the agent and kits on
    /// the second line, what the row needs on a third, and no start folder.
    narrow: bool,
}

impl Columns {
    /// Sized by what the widest entries of *every* sandbox need, drawn or not: the columns must
    /// not move when the list scrolls or the second section folds.
    fn new(ui: &egui::Ui, app: &App, width: f32) -> Self {
        let measure = |text: String, size: f32| {
            ui.painter()
                .layout_no_wrap(text, FontId::proportional(size), Color32::WHITE)
                .size()
                .x
        };
        // The middle column needs the agent: the kits count what they cannot show.
        let mut want = [0.0f32; 3];
        // The most the middle column can use: the agent, or all kits of a sandbox side by side.
        let mut runs_most = 0.0f32;
        for sb in &app.sandboxes {
            let badges = sb.kit_badges();
            let pills: f32 = (badges.iter())
                .map(|(_, label)| measure(label.clone(), BADGE) + 2.0 * BADGE_PAD.x)
                .sum::<f32>()
                + BADGE_GAP * badges.len().saturating_sub(1) as f32;
            runs_most = runs_most.max(pills);
            let waiting = app.approvals_for(&sb.name);
            let attention = sb
                .card_note(waiting)
                .map_or(0.0, |note| measure(note_text(note).0, SECOND))
                + sb.row_markers(waiting).len() as f32 * 18.0;
            want[0] = want[0]
                .max(measure(sb.label.clone(), FIRST))
                .max(sb.start_inside().map_or(0.0, |s| measure(s, SECOND)));
            want[1] = want[1].max(measure(sb.type_label(&app.catalog), FIRST));
            want[2] = want[2]
                .max(measure(sb.status_parts().join(" · "), FIRST))
                .max(attention);
        }
        // A little slack, so a value measured to fit is not cut by rounding.
        let want = want.map(|w| w.ceil() + 2.0);
        let runs_most = (runs_most.ceil() + 2.0).max(want[1]);
        let left = PAD_X + DOT_W;

        if width < NARROW {
            let avail = (width - left - PAD_X - GAP).max(0.0);
            // Only the state: what the row needs goes on a line of its own.
            let status = (app.sandboxes.iter())
                .map(|sb| measure(navigator_status(sb).join(" · "), SECOND))
                .fold(0.0, f32::max);
            let state = (status.ceil() + 2.0).min(avail * 0.4);
            return Columns {
                name: (left, avail - state),
                runs: (left, 0.0),
                state: (width - PAD_X - state, state),
                narrow: true,
            };
        }
        let avail = (width - left - PAD_X - 2.0 * GAP).max(0.0);
        let [name, runs, state] = roster::columns(avail, want);
        // Past what the middle column can use, the room stays at the row's end, after the state.
        let runs = runs.min(runs_most);
        let runs_x = left + name + GAP;
        Columns {
            name: (left, name),
            runs: (runs_x, runs),
            state: (runs_x + runs + GAP, state),
            narrow: false,
        }
    }

    /// How many lines a sandbox's row takes: a second one only for something to put on it — and
    /// in the narrow layout always, the agent's, and a third for what the row needs from you.
    fn lines(&self, sb: &Sandbox, waiting: usize) -> usize {
        match self.narrow {
            true => 2 + usize::from(needs_you(sb, waiting)),
            false => 1 + usize::from(sb.second_line(waiting)),
        }
    }
}

/// Does the row have a note or a marker to show — something to do about it?
fn needs_you(sb: &Sandbox, waiting: usize) -> bool {
    sb.card_note(waiting).is_some() || !sb.row_markers(waiting).is_empty()
}

/// The whole list, in a vertical scroll area. `moved`: this frame's keys moved the selection, so
/// the selected row scrolls into view. `busy`: a session is being prepared (`actions::enabled`).
/// A double click on a sandbox's row starts it, a right click lists its actions.
pub fn list(ui: &mut egui::Ui, app: &mut App, moved: bool, busy: bool) {
    let rows = app.roster();
    // A scroll bar may take its width on the right; leave it room rather than reflow under it.
    let width = (ui.available_width() - ui.spacing().scroll.bar_width).max(0.0);
    let cols = Columns::new(ui, app, width);
    // A page is a screenful of two-line rows.
    app.grid_rows = ((ui.available_height() / ROW_H[1]) as usize).max(1);

    let mut clicked = None;
    let mut toggle = false;
    let mut chosen = None;
    let mut menu = None;
    let view: &App = app;
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for row in &rows {
                match row {
                    Row::Here => here(ui, view, width),
                    Row::Other => toggle |= other(ui, view, width).clicked(),
                    Row::Folder(head) => folder(ui, head, width),
                    Row::Sandbox(cell) => {
                        let Some(sb) = view.sandbox_at(*cell) else {
                            continue;
                        };
                        let selected = *cell == view.cursor;
                        let response = sandbox(ui, view, sb, &cols, width, selected);
                        if response.clicked() {
                            clicked = Some(*cell);
                        }
                        if response.double_clicked() {
                            chosen = Some(Action::Start);
                        }
                        response.context_menu(|ui| {
                            if let Some(action) = actions::row_menu(ui, view, sb, selected, busy) {
                                menu = Some((*cell, action));
                            }
                        });
                        if selected && moved {
                            response.scroll_to_me(None);
                        }
                    }
                    Row::New(cell) => {
                        let selected = *cell == view.cursor;
                        let response = new_row(ui, view, &cols, width, selected);
                        if response.clicked() {
                            clicked = Some(*cell);
                        }
                        if selected && moved {
                            response.scroll_to_me(None);
                        }
                    }
                }
            }
        });
    if let Some(cell) = clicked {
        match app.sandbox_at(cell) {
            Some(_) => app.select(cell),
            // As in the TUI: the *+ New sandbox* row opens the create form.
            None => app.click(Hit::NewCard),
        }
    }
    if let Some((cell, action)) = menu {
        app.select(cell);
        actions::run(app, action, busy);
    }
    if toggle {
        app.perform(Action::ToggleOther);
    }
    if let Some(action) = chosen {
        actions::run(app, action, busy);
    }
}

/// One line of text, cut with `…` where it is wider than `width`.
fn line(ui: &egui::Ui, text: String, size: f32, color: Color32, width: f32) -> Arc<Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text, FontId::proportional(size), color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    ui.painter().layout_job(job)
}

/// How wide `text` is in the regular face at `size`.
fn measure(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    (ui.painter())
        .layout_no_wrap(text.to_string(), FontId::proportional(size), Color32::WHITE)
        .size()
        .x
}

/// `text` as it fits in `room` in `font`: whole, or its end behind a `…` — the start of a path
/// is what a reader can spare.
fn elide_start(ui: &egui::Ui, text: &str, font: &FontId, room: f32) -> String {
    let width = |t: &str| {
        (ui.painter())
            .layout_no_wrap(t.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
    };
    if width(text) <= room {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    (1..=chars.len())
        .map(|cut| format!("…{}", chars[cut..].iter().collect::<String>()))
        .find(|t| width(t) <= room)
        .unwrap_or_default()
}

/// Paint `galley` left-aligned in the column at (`x`, `width`) of the line whose top is `y`,
/// and give it a tooltip with the whole text, `full`, when it was cut — none for an empty `full`,
/// whose caller sets a tooltip of its own. Returns its rect.
fn put(
    ui: &egui::Ui,
    galley: Arc<Galley>,
    full: &str,
    at: (f32, f32),
    y: f32,
    color: Color32,
) -> Rect {
    let pos = Pos2::new(at.0, y + (LINE_H - galley.size().y) / 2.0);
    let rect = Rect::from_min_size(pos, galley.size());
    if galley.elided && !full.is_empty() {
        let id = ui
            .scope_id()
            .with(("cut", full, pos.x as i32, pos.y as i32));
        ui.interact(rect, id, Sense::hover()).on_hover_text(full);
    }
    ui.painter().galley(pos, galley, color);
    rect
}

/// The row's band: the accent tint and a strip down its left edge on the selected row — the
/// selection in shape as well as colour — and a light tint under the pointer.
fn band(ui: &egui::Ui, rect: Rect, selected: bool, hovered: bool) {
    let visuals = ui.visuals();
    let accent = theme::accent(visuals.dark_mode);
    if selected {
        ui.painter()
            .rect_filled(rect, RADIUS, theme::band(visuals.dark_mode));
        let strip = Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height()))
            .shrink2(Vec2::new(0.0, 5.0));
        ui.painter().rect_filled(strip, 1.5, accent);
    } else if hovered {
        // The faint fill of a striped row: a button's hover grey cost the row's text its contrast.
        ui.painter()
            .rect_filled(rect, RADIUS, visuals.faint_bg_color);
    }
}

/// The status dot: filled green while running, a grey ring while stopped — painted, so it never
/// depends on a font having the glyph.
fn dot(ui: &egui::Ui, center: Pos2, running: bool) {
    let dark = ui.visuals().dark_mode;
    match running {
        true => ui
            .painter()
            .circle_filled(center, 4.5, theme::running(dark)),
        false => ui
            .painter()
            .circle_stroke(center, 4.0, Stroke::new(1.5, theme::stopped(dark))),
    };
}

/// A sandbox's row.
fn sandbox(
    ui: &mut egui::Ui,
    app: &App,
    sb: &Sandbox,
    cols: &Columns,
    width: f32,
    selected: bool,
) -> egui::Response {
    let dark = ui.visuals().dark_mode;
    let waiting = app.approvals_for(&sb.name);
    let lines = cols.lines(sb, waiting);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_H[lines - 1]), Sense::CLICK);
    let kind = sb.type_label(&app.catalog);
    let status = sb.status_parts().join(" · ");
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    // One announcement for the row: what a screen reader would otherwise read cell by cell.
    response.widget_info(|| {
        WidgetInfo::selected(
            Role::ListItem,
            true,
            selected,
            format!("{}, {kind}, {status}", sb.label),
        )
    });
    band(ui, rect, selected, response.hovered());

    let x = |col: (f32, f32)| (rect.left() + col.0, col.1);
    let first = rect.top() + PAD_Y;
    let center = Pos2::new(rect.left() + PAD_X + 5.0, first + LINE_H / 2.0);
    dot(ui, center, sb.running);
    let dot_rect = Rect::from_center_size(center, Vec2::splat(14.0));
    ui.interact(
        dot_rect,
        ui.scope_id().with(("dot", &sb.name)),
        Sense::hover(),
    )
    .on_hover_text(sb.status_text());
    if cols.narrow {
        navigator_lines(ui, sb, waiting, cols, rect, &kind);
        return response;
    }

    let strong = ui.visuals().strong_text_color();
    let weak = ui.visuals().weak_text_color();
    let label = line(ui, sb.label.clone(), FIRST, strong, cols.name.1);
    put(ui, label, &sb.label, x(cols.name), first, strong);
    let agent = theme::agent_color(&sb.agent, dark);
    let kind_galley = line(ui, kind.clone(), FIRST, agent, cols.runs.1);
    let second = first + LINE_H;
    put(ui, kind_galley, &kind, x(cols.runs), first, agent);
    let status_colour = match sb.running {
        true => theme::running(dark),
        false => weak,
    };
    let (status, status_full) = squeeze(ui, sb.status_parts(), cols.state.1, FIRST);
    let status = line(ui, status, FIRST, status_colour, cols.state.1);
    put(
        ui,
        status,
        &status_full,
        x(cols.state),
        first,
        status_colour,
    );

    if lines < 2 {
        return response;
    }
    if let Some(inside) = sb.start_inside() {
        let missing = sb.start_dir_missing();
        let colour = match missing {
            true => theme::warn(dark),
            false => weak,
        };
        let galley = line(ui, inside.clone(), SECOND, colour, cols.name.1);
        let shown = put(ui, galley, &inside, x(cols.name), second, colour);
        if missing {
            ui.interact(
                shown,
                ui.scope_id().with(("gone", &sb.name)),
                Sense::hover(),
            )
            .on_hover_text("The start folder is gone: a start opens the workspace root.");
        }
    }
    kits(ui, sb, x(cols.runs), second, dark);
    attention(ui, sb, waiting, x(cols.state), second, dark);
    response
}

/// A navigator's row (the narrow layout): the name with the state at the row's right edge, the
/// agent and its kits on the second line, and what the row needs from you on a third.
fn navigator_lines(
    ui: &egui::Ui,
    sb: &Sandbox,
    waiting: usize,
    cols: &Columns,
    rect: Rect,
    kind: &str,
) {
    let dark = ui.visuals().dark_mode;
    let (left, right) = (rect.left() + cols.name.0, rect.right() - PAD_X);
    let first = rect.top() + PAD_Y;
    let status_colour = match sb.running {
        true => theme::running(dark),
        false => ui.visuals().weak_text_color(),
    };
    let (status, _) = squeeze(ui, navigator_status(sb), cols.state.1, SECOND);
    let status = line(ui, status, SECOND, status_colour, cols.state.1);
    let status_x = right - status.size().x;
    let at = (status_x, cols.state.1);
    let shown = put(ui, status, "", at, first, status_colour);
    // The whole state, always: a stopped row shows only when it was last used.
    let id = ui.scope_id().with(("state", &sb.name));
    let tip = sb.status_parts().join(" · ");
    ui.interact(shown, id, Sense::hover()).on_hover_text(tip);
    let strong = ui.visuals().strong_text_color();
    let room = (status_x - 12.0 - left).max(0.0);
    let label = line(ui, sb.label.clone(), FIRST, strong, room);
    put(ui, label, &sb.label, (left, room), first, strong);

    let second = first + LINE_H;
    let agent = theme::agent_color(&sb.agent, dark);
    let kind_galley = line(ui, kind.to_string(), FIRST, agent, right - left);
    let shown = put(ui, kind_galley, kind, (left, right - left), second, agent);
    let after = shown.right() + 10.0;
    kits(ui, sb, (after, (right - after).max(0.0)), second, dark);
    if needs_you(sb, waiting) {
        attention(ui, sb, waiting, (left, right - left), second + LINE_H, dark);
    }
}

/// What the navigator's row says at its right edge: the state of a running sandbox, and when a
/// stopped one was last used. The whole state is the tooltip.
fn navigator_status(sb: &Sandbox) -> Vec<String> {
    match (sb.running, sb.last_used) {
        (false, Some(epoch)) => vec![aisbx_core::state::ago(epoch)],
        _ => sb.status_parts(),
    }
}

/// `running · 1 session · clone` in the regular face at `size`, dropped from the right until it
/// fits: the first part stays. Returns the shown text and the whole one, for the tooltip.
fn squeeze(ui: &egui::Ui, mut parts: Vec<String>, width: f32, size: f32) -> (String, String) {
    let full = parts.join(" · ");
    let measure = |text: String| {
        ui.painter()
            .layout_no_wrap(text, FontId::proportional(size), Color32::WHITE)
            .size()
            .x
    };
    while parts.len() > 1 && measure(parts.join(" · ")) > width {
        parts.pop();
    }
    (parts.join(" · "), full)
}

/// The kit badges, each tinted in its family's colour, as many as fit; then `+N` for the rest,
/// whose tooltip names them.
fn kits(ui: &egui::Ui, sb: &Sandbox, at: (f32, f32), y: f32, dark: bool) {
    let badges = sb.kit_badges();
    let font = FontId::proportional(BADGE);
    let weak = ui.visuals().weak_text_color();
    let galleys: Vec<Arc<Galley>> = badges
        .iter()
        .map(|(kit, label)| {
            ui.painter()
                .layout_no_wrap(label.clone(), font.clone(), theme::kit_color(kit, dark))
        })
        .collect();
    let widths: Vec<f32> = galleys
        .iter()
        .map(|g| g.size().x + 2.0 * BADGE_PAD.x)
        .collect();
    let more = |n: usize| {
        ui.painter()
            .layout_no_wrap(format!("+{n}"), font.clone(), weak)
            .size()
            .x
            + 2.0 * BADGE_PAD.x
    };
    let Some(shown) = roster::fit_badges(&widths, at.1, BADGE_GAP, more) else {
        return;
    };
    let mut x = at.0;
    let mut pill = |galley: Arc<Galley>, colour: Color32, tip: String| {
        let size = galley.size() + 2.0 * BADGE_PAD;
        let rect = Rect::from_min_size(Pos2::new(x, y + (LINE_H - size.y) / 2.0), size);
        ui.painter()
            .rect_filled(rect, 4.0, theme::badge_fill(colour, dark));
        ui.painter().galley(rect.min + BADGE_PAD, galley, colour);
        let id = ui.scope_id().with(("kit", &sb.name, &tip));
        ui.interact(rect, id, Sense::hover()).on_hover_text(tip);
        x += size.x + BADGE_GAP;
    };
    for ((kit, _), galley) in badges.iter().zip(galleys).take(shown) {
        pill(galley, theme::kit_color(kit, dark), kit.clone());
    }
    if shown < badges.len() {
        let hidden: Vec<&str> = badges[shown..]
            .iter()
            .map(|(kit, _)| kit.as_str())
            .collect();
        let galley = ui
            .painter()
            .layout_no_wrap(format!("+{}", hidden.len()), font.clone(), weak);
        pill(galley, weak, hidden.join(", "));
    }
}

/// Every kit badge of `sb`, wrapping onto more lines where they need them: the page's head shows
/// them all, where a row shows what fits.
pub fn kit_row(ui: &mut egui::Ui, sb: &Sandbox) {
    let dark = ui.visuals().dark_mode;
    let font = FontId::proportional(BADGE + 0.5);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(BADGE_GAP + 1.0);
        for (kit, label) in sb.kit_badges() {
            let colour = theme::kit_color(&kit, dark);
            let galley = ui.painter().layout_no_wrap(label, font.clone(), colour);
            let pad = BADGE_PAD + Vec2::new(1.0, 1.0);
            let (rect, response) =
                ui.allocate_exact_size(galley.size() + 2.0 * pad, Sense::hover());
            ui.painter()
                .rect_filled(rect, 4.0, theme::badge_fill(colour, dark));
            ui.painter().galley(rect.min + pad, galley, colour);
            response.on_hover_text(kit);
        }
    });
}

/// What the sandbox needs from you: the note in the danger or warning colour, then a marker for
/// every other condition — each with a tooltip naming the key that fixes it.
fn attention(ui: &egui::Ui, sb: &Sandbox, waiting: usize, at: (f32, f32), y: f32, dark: bool) {
    let markers = sb.row_markers(waiting);
    let mut x = at.0;
    let right = at.0 + at.1;
    if let Some(note) = sb.card_note(waiting) {
        let (text, fix) = note_text(note);
        let colour = match note.is_danger() {
            true => theme::danger(dark),
            false => theme::warn(dark),
        };
        let room = (right - x - markers.len() as f32 * 18.0).max(0.0);
        let galley = line(ui, text, SECOND, colour, room);
        let rect = put(ui, galley, "", (x, room), y, colour);
        let id = ui.scope_id().with(("note", &sb.name));
        let fix = match (&sb.drift, note) {
            (Some(drift), CardNote::KitsChanged(_)) => format!("{} {fix}", drift.reason()),
            _ => fix.to_string(),
        };
        ui.interact(rect, id, Sense::hover()).on_hover_text(fix);
        x = rect.right() + 8.0;
    }
    for marker in markers {
        let (glyph, mut why) = marker_text(marker, waiting);
        if let (Some(drift), CardMarker::KitsChanged) = (&sb.drift, marker) {
            why = format!("{} {why}", drift.reason());
        }
        let galley = line(ui, glyph.into(), SECOND, theme::warn(dark), right - x);
        let rect = put(ui, galley, "", (x, 18.0), y, theme::warn(dark));
        let id = ui.scope_id().with(("marker", &sb.name, glyph));
        ui.interact(rect, id, Sense::hover()).on_hover_text(why);
        x = rect.right() + 6.0;
    }
}

/// A folder as a heading writes it: weak up to its last part, the last part strong — the folder
/// the sandboxes are named after — in at most `room`: where it does not fit, the start gives way
/// to `…`, and the folder's own name stays. Returns the right edge.
fn folder_text(ui: &egui::Ui, path: &std::path::Path, size: f32, pos: Pos2, room: f32) -> f32 {
    let (above, last) = roster::folder_parts(path);
    let font = FontId::proportional(size);
    let weak = ui.visuals().weak_text_color();
    let strong = ui.visuals().strong_text_color();
    let last = line(ui, last, size, strong, room);
    let above = elide_start(ui, &above, &font, (room - last.size().x).max(0.0));
    let above = ui.painter().layout_no_wrap(above, font.clone(), weak);
    let x = pos.x + above.size().x;
    ui.painter().galley(pos, above, weak);
    let right = x + last.size().x;
    ui.painter().galley(Pos2::new(x, pos.y), last, strong);
    right
}

/// The current folder's heading: `This folder  ~/work/proj  3 sandboxes`.
fn here(ui: &mut egui::Ui, app: &App, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::hover());
    let weak = ui.visuals().weak_text_color();
    let title = ui.painter().layout_no_wrap(
        roster::HERE_HEADING.into(),
        FontId::new(HEADING, theme::strong_family()),
        ui.visuals().strong_text_color(),
    );
    let base = rect.left_bottom() + Vec2::new(PAD_X, -10.0);
    let title_pos = Pos2::new(base.x, base.y - title.size().y);
    let mut x = title_pos.x + title.size().x + 12.0;
    ui.painter()
        .galley(title_pos, title, ui.visuals().strong_text_color());
    let path_y = base.y - ui.text_style_height(&egui::TextStyle::Body) - 1.0;
    let count = match app.here {
        0 => "no sandboxes yet".to_string(),
        n => roster::count(n),
    };
    let count = ui
        .painter()
        .layout_no_wrap(count, FontId::proportional(SECOND), weak);
    let room = rect.right() - PAD_X - x - 12.0 - count.size().x;
    x = folder_text(ui, &app.cwd, 13.5, Pos2::new(x, path_y), room);
    ui.painter()
        .galley(Pos2::new(x + 12.0, path_y + 1.0), count, weak);
}

/// The other folders' heading — a button that folds the section away and back (`a`). The count
/// stands after the title where it fits, else on a line of its own under it.
fn other(ui: &mut egui::Ui, app: &App, width: f32) -> egui::Response {
    let count = roster::other_count(app.elsewhere, app.other_folders());
    let title_w = measure_strong(ui, roster::OTHER_HEADING, HEADING);
    let beside = PAD_X + DOT_W + title_w + 12.0 + measure(ui, &count, SECOND) + PAD_X <= width;
    let height = if beside { 50.0 } else { 50.0 + LINE_H };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::CLICK);
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text(match app.show_other {
            true => "Click to hide them (a)",
            false => "Click to show them (a)",
        });
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, roster::OTHER_HEADING));
    let visuals = ui.visuals();
    let colour = match response.hovered() {
        true => visuals.strong_text_color(),
        false => visuals.text_color(),
    };
    let rule_y = rect.top() + 12.0;
    ui.painter().hline(
        rect.left() + PAD_X..=rect.right() - PAD_X,
        rule_y,
        visuals.widgets.noninteractive.bg_stroke,
    );
    let title = ui.painter().layout_no_wrap(
        roster::OTHER_HEADING.into(),
        FontId::new(HEADING, theme::strong_family()),
        colour,
    );
    let line_top = rect.top() + 50.0 - 10.0 - title.size().y;
    // The disclosure triangle, painted: ▾ open, ▸ folded.
    let tip = Pos2::new(rect.left() + PAD_X + 5.0, line_top + title.size().y / 2.0);
    let triangle = match app.show_other {
        true => vec![
            tip + Vec2::new(-5.0, -3.0),
            tip + Vec2::new(5.0, -3.0),
            tip + Vec2::new(0.0, 4.0),
        ],
        false => vec![
            tip + Vec2::new(-3.0, -5.0),
            tip + Vec2::new(-3.0, 5.0),
            tip + Vec2::new(4.0, 0.0),
        ],
    };
    ui.painter()
        .add(egui::Shape::convex_polygon(triangle, colour, Stroke::NONE));
    let x = rect.left() + PAD_X + DOT_W;
    ui.painter().galley(Pos2::new(x, line_top), title, colour);
    let weak = visuals.weak_text_color();
    let count = ui
        .painter()
        .layout_no_wrap(count, FontId::proportional(SECOND), weak);
    let at = match beside {
        true => Pos2::new(
            x + title_w + 12.0,
            line_top + (HEADING - SECOND) / 2.0 + 1.0,
        ),
        false => Pos2::new(x, rect.top() + 50.0 - 6.0),
    };
    ui.painter().galley(at, count, weak);
    response
}

/// How wide `text` is in the strong face at `size`.
fn measure_strong(ui: &egui::Ui, text: &str, size: f32) -> f32 {
    let font = FontId::new(size, theme::strong_family());
    (ui.painter()
        .layout_no_wrap(text.into(), font, Color32::WHITE))
    .size()
    .x
}

/// A folder heading in the second section, over the sandboxes that mount it: a hairline that
/// sets it apart from the folder above, the folder's mark and the folder, then how many
/// sandboxes mount it.
fn folder(ui: &mut egui::Ui, head: &FolderHead, width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 40.0), Sense::hover());
    let visuals = ui.visuals();
    ui.painter().hline(
        rect.left() + PAD_X + DOT_W..=rect.right() - PAD_X,
        rect.top() + 8.0,
        Stroke::new(
            1.0,
            visuals
                .widgets
                .noninteractive
                .bg_stroke
                .color
                .gamma_multiply(0.6),
        ),
    );
    let strong = visuals.strong_text_color();
    let weak = visuals.weak_text_color();
    let y = rect.bottom() - 22.0;
    let mark = ui
        .painter()
        .layout_no_wrap("🗁".into(), FontId::proportional(14.0), weak);
    let mark_x = rect.left() + PAD_X + (DOT_W - mark.size().x) / 2.0;
    ui.painter().galley(Pos2::new(mark_x, y), mark, weak);
    let pos = Pos2::new(rect.left() + PAD_X + DOT_W, y);
    let count = roster::count(head.sandboxes);
    let gone = head.missing.then_some(roster::FOLDER_GONE);
    let tail =
        12.0 + measure(ui, &count, SECOND) + gone.map_or(0.0, |g| 10.0 + measure(ui, g, SECOND));
    let room = rect.right() - PAD_X - pos.x - tail;
    let mut right = match head.mountless() {
        true => {
            let galley = line(ui, roster::NO_FOLDER.into(), 14.0, strong, room);
            let right = pos.x + galley.size().x;
            ui.painter().galley(pos, galley, strong);
            right
        }
        false => folder_text(ui, &head.path, 14.0, pos, room),
    };
    if let Some(gone) = gone {
        let colour = theme::danger(visuals.dark_mode);
        let galley = ui
            .painter()
            .layout_no_wrap(gone.into(), FontId::proportional(SECOND), colour);
        let at = Pos2::new(right + 10.0, pos.y + 1.5);
        right = at.x + galley.size().x;
        ui.painter().galley(at, galley, colour);
    }
    let galley = ui
        .painter()
        .layout_no_wrap(count, FontId::proportional(SECOND), weak);
    ui.painter()
        .galley(Pos2::new(right + 12.0, pos.y + 1.5), galley, weak);
}

/// The "+ New sandbox" row closing the current folder's section; it lights up only when
/// selected, like every other row.
fn new_row(
    ui: &mut egui::Ui,
    app: &App,
    cols: &Columns,
    width: f32,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_H[0]), Sense::CLICK);
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .on_hover_text("Create a new sandbox for this folder (n)");
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, selected, "New sandbox"));
    band(ui, rect, selected, response.hovered());
    let dark = ui.visuals().dark_mode;
    let y = rect.top() + PAD_Y;
    // The plus, painted like the dot: no font has to have it.
    let center = Pos2::new(rect.left() + PAD_X + 5.0, y + LINE_H / 2.0);
    let stroke = Stroke::new(2.0, theme::accent(dark));
    ui.painter().line_segment(
        [center - Vec2::new(5.0, 0.0), center + Vec2::new(5.0, 0.0)],
        stroke,
    );
    ui.painter().line_segment(
        [center - Vec2::new(0.0, 5.0), center + Vec2::new(0.0, 5.0)],
        stroke,
    );
    let strong = ui.visuals().strong_text_color();
    let title = line(
        ui,
        "New sandbox".into(),
        FIRST,
        strong,
        cols.name.1.max(120.0),
    );
    let shown = put(
        ui,
        title,
        "New sandbox",
        (rect.left() + cols.name.0, 0.0),
        y,
        strong,
    );
    let hint = match app.here == 0 {
        true => "none here yet",
        false => "for this folder",
    };
    let weak = ui.visuals().weak_text_color();
    // With no sandbox to size the columns, the first one is narrower than the title.
    let x = match cols.narrow {
        true => shown.right() + 12.0,
        false => (rect.left() + cols.runs.0).max(shown.right() + 16.0),
    };
    let hint = line(ui, hint.into(), SECOND, weak, rect.right() - PAD_X - x);
    put(ui, hint, "", (x, 0.0), y, weak);
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use aisbx_core::kits::Catalog;
    use aisbx_core::model;

    #[test]
    fn every_note_names_its_fix_by_the_windows_key() {
        for (note, key) in [
            (CardNote::Unresponsive, " s,"),
            (CardNote::NoToken, " A."),
            (CardNote::Approvals(2), " y."),
            (CardNote::KitsChanged(model::DriftKind::Changed), " R."),
        ] {
            let (text, fix) = note_text(note);
            assert!(!text.is_empty());
            assert!(
                fix.contains(key) && !fix.contains("aisbx"),
                "{note:?}: {fix}"
            );
        }
        assert_eq!(note_text(CardNote::Approvals(3)).0, "⚑ 3 to approve");
        for marker in [CardMarker::Approvals, CardMarker::KitsChanged] {
            let (_, why) = marker_text(marker, 2);
            assert!(why.ends_with(" y.") || why.ends_with(" R."), "{why}");
        }
    }

    fn mixed() -> App {
        let mut sandboxes = vec![
            model::sample("here-1", Some("claude-anthropic"), &[]),
            model::sample("here-2", Some("claude-anthropic"), &[]),
        ];
        for (i, dir) in ["/work/api", "/work/api", "/work/ui"].iter().enumerate() {
            let mut sb = model::sample(&format!("far-{i}"), Some("claude-anthropic"), &[]);
            sb.workspace = std::path::PathBuf::from(dir);
            sandboxes.push(sb);
        }
        App::for_test(Catalog::load().unwrap(), sandboxes)
    }

    /// One frame of the list alone, at `width`×760, with these events; the texts it drew.
    fn frame(
        ctx: &egui::Context,
        app: &mut App,
        width: f32,
        events: Vec<egui::Event>,
    ) -> Vec<(String, Rect)> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 760.0))),
            events,
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| list(ui, app, true, false));
        output.textures_delta.clear();
        let mut drawn = Vec::new();
        fn collect(shape: &egui::Shape, out: &mut Vec<(String, Rect)>) {
            match shape {
                egui::Shape::Text(text) => {
                    let rect = text.galley.rect.translate(text.pos.to_vec2());
                    out.push((text.galley.text().to_string(), rect));
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut drawn);
        }
        drawn
    }

    fn at<'a>(drawn: &'a [(String, Rect)], text: &str) -> &'a Rect {
        &drawn
            .iter()
            .find(|(t, _)| t == text)
            .unwrap_or_else(|| panic!("{text} in {drawn:?}"))
            .1
    }

    #[test]
    fn a_row_draws_every_fact_in_its_column() {
        let mut a = model::sample("gw", Some("claude-litellm"), &["jdk25", "rtk", "web"]);
        a.label = "Gateway Claude".into();
        a.running = true;
        a.sessions = 2;
        a.start_dir = Some("api".into());
        a.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        let b = model::sample("login", Some("claude-anthropic"), &[]);
        let mut app = App::for_test(Catalog::load().unwrap(), vec![a, b]);
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, 900.0, Vec::new());
        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        for text in [
            "Gateway Claude",
            "Claude Code · LiteLLM acme",
            "running · 2 sessions",
            "› api",
            "JDK25",
            "RTK",
            "Web",
            "⟳ update ready",
            "Claude Code · /login",
            "stopped",
        ] {
            at(&drawn, text);
        }
        let left = |t: &str| at(&drawn, t).left();
        assert_eq!(
            left("Claude Code · LiteLLM acme"),
            left("Claude Code · /login")
        );
        assert_eq!(left("running · 2 sessions"), left("stopped"));
        assert_eq!(
            left("JDK25"),
            left("Claude Code · LiteLLM acme") + BADGE_PAD.x
        );
        assert_eq!(left("⟳ update ready"), left("stopped"));
        assert!(at(&drawn, "› api").top() > at(&drawn, "Gateway Claude").bottom() - 1.0);
    }

    #[test]
    fn a_wide_list_keeps_the_state_beside_what_it_describes() {
        let a = model::sample("gw", Some("claude-litellm"), &["jdk25", "rtk", "web"]);
        let mut b = model::sample("login", Some("claude-anthropic"), &[]);
        b.running = true;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![a, b]);
        let ctx = egui::Context::default();
        for width in [1600.0, 640.0] {
            frame(&ctx, &mut app, width, Vec::new());
            let drawn = frame(&ctx, &mut app, width, Vec::new());
            let middle = ["Claude Code · LiteLLM acme", "Claude Code · /login"]
                .iter()
                .map(|t| at(&drawn, t).right())
                .chain([at(&drawn, "Web").right() + BADGE_PAD.x])
                .fold(0.0, f32::max);
            let state = at(&drawn, "stopped").left();
            assert_eq!(state, at(&drawn, "running").left(), "{width}: one column");
            let gap = state - middle;
            assert!(gap >= GAP, "{width}: {gap}");
            if width > 1000.0 {
                assert!(gap <= GAP + 4.0, "{width}: beside it, {gap}");
            }
        }
    }

    #[test]
    fn other_folders_sit_under_their_folder_headings() {
        let mut app = mixed();
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, 900.0, Vec::new());
        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        let y = |t: &str| at(&drawn, t).top();
        let heading = roster::OTHER_HEADING;
        assert!(y(heading) < y("api") && y("api") < y("far-0"));
        assert!(y("far-1") < y("ui") && y("ui") < y("far-2"));
        at(&drawn, "3 sandboxes in 2 folders");
        assert_eq!(drawn.iter().filter(|(t, _)| t == "api").count(), 1);
        for (folder, count) in [("api", "2 sandboxes"), ("ui", "1 sandbox")] {
            let line = |t: &str| {
                (drawn.iter())
                    .filter(|(text, r)| text == t && (r.center().y - y(folder)).abs() < 12.0)
                    .count()
            };
            assert_eq!(line(count), 1, "{folder}: {drawn:?}");
            assert_eq!(line("🗁"), 1, "{folder}: {drawn:?}");
        }
    }

    #[test]
    fn the_other_folders_count_goes_under_its_heading_where_it_has_no_room_beside_it() {
        let mut app = mixed();
        let ctx = egui::Context::default();
        for (width, beside) in [(900.0, true), (300.0, false)] {
            frame(&ctx, &mut app, width, Vec::new());
            let drawn = frame(&ctx, &mut app, width, Vec::new());
            let heading = at(&drawn, roster::OTHER_HEADING);
            let count = at(&drawn, "3 sandboxes in 2 folders");
            assert_eq!(count.top() < heading.bottom(), beside, "{width}");
            assert!(count.right() <= width, "{width}: {count:?}");
        }
    }

    #[test]
    fn clicks_select_create_and_fold() {
        let mut app = mixed();
        let ctx = egui::Context::default();
        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        let click = |ctx: &egui::Context, app: &mut App, pos: Pos2| {
            let button = |pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            frame(
                ctx,
                app,
                900.0,
                vec![egui::Event::PointerMoved(pos), button(true)],
            );
            frame(ctx, app, 900.0, vec![button(false)])
        };
        let far = at(&drawn, "far-2").center();
        click(&ctx, &mut app, far);
        assert_eq!(app.selected().map(|sb| sb.name.as_str()), Some("far-2"));

        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        let heading = at(&drawn, roster::OTHER_HEADING).center();
        click(&ctx, &mut app, heading);
        assert!(!app.show_other, "the heading folded the section");

        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        let new = at(&drawn, "New sandbox").center();
        click(&ctx, &mut app, new);
        assert!(matches!(app.screen, aisbx_core::app::Screen::Create(_)));
    }

    #[test]
    fn the_new_rows_hint_never_runs_over_its_title() {
        let one = vec![model::sample("a", Some("claude-anthropic"), &[])];
        for sandboxes in [Vec::new(), one] {
            let mut app = App::for_test(Catalog::load().unwrap(), sandboxes);
            for cell in [0, app.here] {
                app.select(cell);
                let ctx = egui::Context::default();
                frame(&ctx, &mut app, 900.0, Vec::new());
                let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
                let title = *at(&drawn, "New sandbox");
                let hint = drawn
                    .iter()
                    .find(|(t, r)| {
                        t != "New sandbox"
                            && (r.center().y - title.center().y).abs() < 4.0
                            && r.center().x > title.center().x
                    })
                    .unwrap_or_else(|| panic!("no hint beside the title: {drawn:?}"))
                    .1;
                assert!(hint.left() > title.right(), "{title:?} under {hint:?}");
            }
        }
    }

    /// The texts one frame of the list at `width` cut with `…`.
    fn cut(ctx: &egui::Context, app: &mut App, width: f32) -> Vec<String> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 760.0))),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| list(ui, app, true, false));
        fn collect(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(text) if text.galley.elided => {
                    out.push(text.galley.text().to_string())
                }
                egui::Shape::Vec(shapes) => shapes.iter().for_each(|s| collect(s, out)),
                _ => {}
            }
        }
        let mut cut = Vec::new();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut cut);
        }
        cut
    }

    /// At `shell::LIST_MIN`, the narrowest a drag leaves: the state stands at the row's right
    /// edge, and what the row needs is on a line of its own under the agent.
    #[test]
    fn a_navigator_row_gives_the_agent_the_whole_line() {
        let mut gw = model::sample("gw", Some("claude-litellm"), &["jdk25"]);
        gw.label = "Gateway Claude".into();
        gw.drift = Some(aisbx_core::model::Drift::Changed(vec!["Statusline".into()]));
        let mut login = model::sample("login", Some("claude-anthropic"), &[]);
        login.running = true;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![gw, login]);
        let ctx = egui::Context::default();
        let width = crate::shell::LIST_MIN;
        frame(&ctx, &mut app, width, Vec::new());
        let drawn = frame(&ctx, &mut app, width, Vec::new());
        let agent = "Claude Code · LiteLLM acme";
        assert!(
            !cut(&ctx, &mut app, width).contains(&agent.to_string()),
            "the agent is cut"
        );
        let (label, agent) = (at(&drawn, "Gateway Claude"), at(&drawn, agent));
        assert!(agent.top() > label.bottom() - 1.0, "{label:?} {agent:?}");
        let note = at(&drawn, "⟳ update ready");
        assert!(note.top() > agent.bottom() - 1.0, "{agent:?} {note:?}");
        assert!(
            (note.left() - label.left()).abs() < 1.0,
            "{label:?} {note:?}"
        );
        let (stopped, running) = (at(&drawn, "stopped"), at(&drawn, "running"));
        assert!((stopped.right() - running.right()).abs() < 0.5);
        assert!(stopped.left() > label.right(), "{label:?} {stopped:?}");
    }

    /// A running row keeps its state, and a stopped row without a last use still says *stopped*.
    #[test]
    fn a_stopped_navigator_row_says_when_it_was_last_used() {
        let mut used = model::sample("used", Some("claude-anthropic"), &[]);
        used.last_used = Some(aisbx_core::state::now_epoch() - 3 * 86_400);
        let never = model::sample("never", Some("claude-anthropic"), &[]);
        let mut live = model::sample("live", Some("claude-anthropic"), &[]);
        live.running = true;
        let mut app = App::for_test(Catalog::load().unwrap(), vec![used, never, live]);
        let ctx = egui::Context::default();
        let width = crate::shell::LIST_MIN;
        frame(&ctx, &mut app, width, Vec::new());
        let drawn = frame(&ctx, &mut app, width, Vec::new());
        let texts: Vec<&str> = drawn.iter().map(|(t, _)| t.as_str()).collect();
        assert!(texts.contains(&"3 days ago"), "{texts:?}");
        assert_eq!(texts.iter().filter(|t| **t == "stopped").count(), 1);
        assert!(texts.contains(&"running"), "{texts:?}");
        let drawn = frame(&ctx, &mut app, 900.0, Vec::new());
        assert!(drawn.iter().any(|(t, _)| t == "stopped · 3 days ago"));
    }

    #[test]
    fn a_narrow_list_folds_the_middle_column_under_the_name() {
        let mut a = model::sample("gw", Some("claude-litellm"), &["jdk25"]);
        a.label = "Gateway Claude".into();
        let mut app = App::for_test(Catalog::load().unwrap(), vec![a]);
        let ctx = egui::Context::default();
        frame(&ctx, &mut app, 480.0, Vec::new());
        let drawn = frame(&ctx, &mut app, 480.0, Vec::new());
        let label = at(&drawn, "Gateway Claude");
        let agent = at(&drawn, "Claude Code · LiteLLM acme");
        assert_eq!(label.left(), agent.left());
        assert!(agent.top() > label.top());
        let kit = at(&drawn, "JDK25");
        assert!(kit.left() > agent.right() && (kit.center().y - agent.center().y).abs() < 2.0);
    }
}

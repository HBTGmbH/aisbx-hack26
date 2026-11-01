//! A list of like things on a sandbox's page (ports, rules, hosts, folders), as a desktop window
//! lists them: a [`heading`] with the count and the list's buttons, then an outlined [`table`].
//! Core decides what a row says; this module lays it out.

use std::sync::Arc;

use aisbx_core::actions::Action;
use aisbx_core::app::App;
use eframe::egui::{
    self, Color32, CornerRadius, FontId, Galley, Pos2, Rect, RichText, Role, Sense, Stroke, Vec2,
    WidgetInfo,
};

use crate::{actions, theme};

/// The room inside a row at its sides, above and below its lines, and between two columns.
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 8.0;
const GAP: f32 = 20.0;
/// One line of a row, and the header's height.
const LINE_H: f32 = 22.0;
const HEAD_H: f32 = 30.0;
/// The table's corners, and the first column's least width.
const RADIUS: u8 = 8;
const FIRST_MIN: f32 = 120.0;
/// A badge's padding, and the room between two badges.
const BADGE_PAD: Vec2 = Vec2::new(7.0, 2.0);
const BADGE_GAP: f32 = 6.0;

/// What a cell holds, and how it is drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub text: String,
    /// An exact name — a host, a URL, a path, a port: in the monospace face, as sbx writes it.
    pub mono: bool,
    /// Its colour; `None` is the text colour.
    pub color: Option<Color32>,
    /// A number, which lines up at its column's right edge.
    pub number: bool,
}

impl Cell {
    /// Words, in the text colour.
    pub fn text(text: impl Into<String>) -> Self {
        Cell {
            text: text.into(),
            mono: false,
            color: None,
            number: false,
        }
    }

    /// An exact name, in the monospace face.
    pub fn mono(text: impl Into<String>) -> Self {
        Cell {
            mono: true,
            ..Cell::text(text)
        }
    }

    /// A count, at its column's right edge.
    pub fn number(text: impl Into<String>) -> Self {
        Cell {
            number: true,
            ..Cell::text(text)
        }
    }

    pub fn colored(self, color: Color32) -> Self {
        Cell {
            color: Some(color),
            ..self
        }
    }
}

/// A tag at a row's right edge — who put a rule there, a port that moves, a folder's access — in
/// its colour on its own tint, as the kits' badges are.
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    pub text: String,
    pub color: Color32,
    /// What it means, on hover.
    pub tip: String,
}

/// A row's own button: enabled, or greyed out with the reason as its tooltip.
#[derive(Debug, Clone, PartialEq)]
pub struct Button {
    pub label: &'static str,
    pub state: Result<(), String>,
    pub tip: String,
}

impl Button {
    /// The button for `action` on this row: titled `label`, enabled as the action is.
    pub fn of(app: &App, action: Action, label: &'static str) -> Self {
        Button {
            label,
            state: actions::enabled(app, action, false),
            tip: format!("{} ({})", action.summary(), actions::key_label(action)),
        }
    }
}

/// One thing in a table.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// One per column; the first takes what the others leave, and wraps there.
    pub cells: Vec<Cell>,
    /// More about it, in the weak colour under the first cell: why a request was blocked.
    pub sub: Option<String>,
    pub badges: Vec<Badge>,
    pub button: Option<Button>,
}

impl Row {
    pub fn new(cells: Vec<Cell>) -> Self {
        Row {
            cells,
            sub: None,
            badges: Vec::new(),
            button: None,
        }
    }

    pub fn sub(self, sub: impl Into<String>) -> Self {
        let sub = Some(sub.into()).filter(|s| !s.is_empty());
        Row { sub, ..self }
    }

    pub fn badge(mut self, text: impl Into<String>, color: Color32, tip: &str) -> Self {
        let (text, tip) = (text.into(), tip.to_string());
        self.badges.push(Badge { text, color, tip });
        self
    }

    pub fn button(self, button: Button) -> Self {
        let button = Some(button);
        Row { button, ..self }
    }
}

/// A table's heading: its title, after a dot in `marker`'s colour where the list is one side of a
/// decision (blocked or allowed), the count, muted, and the list's buttons at the right edge,
/// disabled with the reason when their action cannot run. Returns the action a button asked for.
pub fn heading(
    ui: &mut egui::Ui,
    app: &App,
    title: &str,
    count: &str,
    marker: Option<Color32>,
    buttons: &[Action],
) -> Option<Action> {
    let mut clicked = None;
    // Apart from what is above it, but not from the top of the tab.
    if ui.cursor().top() > ui.max_rect().top() + 1.0 {
        ui.add_space(14.0);
    }
    ui.horizontal(|ui| {
        ui.set_min_height(ui.spacing().interact_size.y + 4.0);
        if let Some(color) = marker {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
            ui.painter().circle_filled(rect.center(), 4.0, color);
        }
        ui.label(theme::strong(RichText::new(title).size(theme::SECTION)));
        if !count.is_empty() {
            ui.label(RichText::new(count).size(theme::SMALL + 0.5).weak());
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for action in buttons.iter().rev() {
                let state = actions::enabled(app, *action, false);
                let response = ui
                    .add_enabled(
                        state.is_ok(),
                        egui::Button::new(actions::title(app, *action)),
                    )
                    .on_hover_text(format!(
                        "{} ({})",
                        action.summary(),
                        actions::key_label(*action)
                    ));
                match state {
                    Ok(()) if response.clicked() => clicked = Some(*action),
                    Ok(()) => {}
                    Err(why) => {
                        response.on_disabled_hover_text(theme::with_code(ui, &why, |t| t));
                    }
                }
            }
        });
    });
    ui.add_space(4.0);
    clicked
}

/// `n` things, as a heading counts them: `1 rule`, `3 rules`.
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// `rows` in an outlined table, under `columns`' titles in a header row (none when every title
/// is empty), with a hairline between two rows; `empty`, muted, in place of rows when there is
/// none. Returns the row whose button was clicked.
pub fn table(
    ui: &mut egui::Ui,
    id: &str,
    columns: &[&str],
    rows: &[Row],
    empty: &str,
) -> Option<usize> {
    aligned(ui, id, columns, rows, empty, &[rows])
}

/// [`table`], its columns sized by every row of `sizing`: the tables of a tab that share their
/// columns, so that they line up one under the other.
pub fn aligned(
    ui: &mut egui::Ui,
    id: &str,
    columns: &[&str],
    rows: &[Row],
    empty: &str,
    sizing: &[&[Row]],
) -> Option<usize> {
    let dark = ui.visuals().dark_mode;
    let line = Stroke::new(1.0, theme::line(dark));
    let mut clicked = None;
    egui::Frame::new()
        .stroke(line)
        .corner_radius(RADIUS)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let width = ui.available_width();
            ui.set_width(width);
            let layout = Layout::new(ui, width, columns, sizing);
            if columns.iter().any(|c| !c.is_empty()) {
                header(ui, &layout, columns, width);
            }
            if rows.is_empty() {
                let weak = ui.visuals().weak_text_color();
                let text = galley(ui, empty, false, Some(width - 2.0 * PAD_X));
                let height = text.size().y.max(LINE_H) + 2.0 * PAD_Y;
                let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
                let pos = Pos2::new(rect.left() + PAD_X, rect.top() + PAD_Y);
                ui.painter().galley(pos, text, weak);
            }
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    let y = ui.cursor().top();
                    let x = ui.cursor().left();
                    ui.painter().hline(x..=x + width, y, line);
                }
                if draw_row(ui, &layout, (id, i), row, width) {
                    clicked = Some(i);
                }
            }
        });
    clicked
}

/// Where a table's columns start and how wide each is: each as wide as its widest entry, the first
/// at most what the others, the badges and the button leave (it wraps there). The rest of the room
/// stays before the badges, so a column stands beside the one it follows.
struct Layout {
    x: Vec<f32>,
    widths: Vec<f32>,
}

impl Layout {
    fn new(ui: &egui::Ui, width: f32, columns: &[&str], sizing: &[&[Row]]) -> Self {
        let rows = || sizing.iter().flat_map(|rows| rows.iter());
        let n = (rows().map(|r| r.cells.len()))
            .chain([columns.len(), 1])
            .max()
            .unwrap_or(1);
        let mut widths = vec![0.0f32; n];
        for (i, title) in columns.iter().enumerate() {
            widths[i] = widths[i].max(head_galley(ui, title).size().x);
        }
        for row in rows() {
            for (i, cell) in row.cells.iter().enumerate() {
                widths[i] = widths[i].max(cell_galley(ui, cell, None).size().x);
            }
            if let Some(sub) = &row.sub {
                widths[0] = widths[0].max(galley(ui, sub, false, None).size().x);
            }
        }
        // A little slack, so a cell measured to fit is not wrapped by rounding.
        let mut widths: Vec<f32> = widths.iter().map(|w| w.ceil() + 2.0).collect();
        let right = rows().map(|r| right_width(ui, r)).fold(0.0, f32::max);
        let others: f32 = widths[1..].iter().map(|w| w + GAP).sum();
        let tail = if right > 0.0 { right + GAP } else { 0.0 };
        let room = (width - 2.0 * PAD_X - others - tail).max(FIRST_MIN);
        widths[0] = widths[0].min(room);
        let mut x = Vec::with_capacity(n);
        let mut at = PAD_X;
        for w in &widths {
            x.push(at);
            at += w + GAP;
        }
        Layout { x, widths }
    }
}

/// The header: each column's title, small and muted, on the stripes' fill under the table's
/// rounded top, and a hairline under it.
fn header(ui: &mut egui::Ui, layout: &Layout, columns: &[&str], width: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, HEAD_H), Sense::hover());
    let top = CornerRadius {
        nw: RADIUS,
        ne: RADIUS,
        sw: 0,
        se: 0,
    };
    let visuals = ui.visuals();
    ui.painter().rect_filled(rect, top, visuals.faint_bg_color);
    let weak = visuals.weak_text_color();
    let line = Stroke::new(1.0, theme::line(visuals.dark_mode));
    ui.painter()
        .hline(rect.x_range(), rect.bottom() - 0.5, line);
    for (i, title) in columns.iter().enumerate() {
        let text = head_galley(ui, title);
        let x = rect.left() + layout.x[i];
        let pos = Pos2::new(x, rect.center().y - text.size().y / 2.0);
        ui.painter().galley(pos, text, weak);
    }
}

/// One row: its cells at their columns, the first line of each on one baseline, `sub` under the
/// first cell, the badges and the button from the right edge. Returns whether its button was
/// clicked.
fn draw_row(ui: &mut egui::Ui, layout: &Layout, id: (&str, usize), row: &Row, width: f32) -> bool {
    let dark = ui.visuals().dark_mode;
    let Some(lead) = row.cells.first() else {
        return false;
    };
    let first = cell_galley(ui, lead, Some(layout.widths[0]));
    let weak = ui.visuals().weak_text_color();
    let sub = (row.sub.as_deref()).map(|s| galley(ui, s, false, Some(layout.widths[0])));
    let mut height = first.size().y.max(LINE_H);
    if let Some(sub) = &sub {
        height += sub.size().y + 2.0;
    }
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(width, height + 2.0 * PAD_Y), Sense::hover());
    // One announcement for the row: a screen reader reads no painted text.
    response.widget_info(|| {
        let mut words: Vec<&str> = row.cells.iter().map(|c| c.text.as_str()).collect();
        words.extend(row.sub.as_deref());
        words.extend(row.badges.iter().map(|b| b.text.as_str()));
        WidgetInfo::labeled(Role::Row, true, words.join(", "))
    });
    let line_top = rect.top() + PAD_Y;
    // The first line of every cell centred on one line of LINE_H, so a monospace cell and a
    // proportional one share a baseline closely enough.
    let place = |galley: &Arc<Galley>, x: f32| {
        let line_h = galley.size().y / galley.rows.len().max(1) as f32;
        Pos2::new(x, line_top + (LINE_H - line_h) / 2.0)
    };
    let text_color = ui.visuals().text_color();
    let first_h = first.size().y.max(LINE_H);
    let pos = place(&first, rect.left() + layout.x[0]);
    let color = lead.color.unwrap_or(text_color);
    ui.painter().galley(pos, first, color);
    if let Some(sub) = sub {
        let pos = Pos2::new(rect.left() + layout.x[0], line_top + first_h + 2.0);
        ui.painter().galley(pos, sub, weak);
    }
    for (i, cell) in row.cells.iter().enumerate().skip(1) {
        let text = cell_galley(ui, cell, None);
        let mut x = rect.left() + layout.x[i];
        if cell.number {
            x += layout.widths[i] - text.size().x;
        }
        let color = cell.color.unwrap_or(text_color);
        ui.painter().galley(place(&text, x), text, color);
    }

    // From the right edge: the button, then the badges right to left.
    let centre = line_top + LINE_H / 2.0;
    let mut right = rect.right() - PAD_X;
    let mut clicked = false;
    if let Some(button) = &row.button {
        let size = button_size(ui, button.label);
        let at = Rect::from_min_size(Pos2::new(right - size.x, centre - size.y / 2.0), size);
        // In a child of its own: placed on the row, it moves nothing after it.
        let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(at).id_salt((id, "button")));
        let edge = Stroke::new(1.0, theme::line(dark));
        let widget = egui::Button::new(button.label)
            .stroke(edge)
            .wrap_mode(egui::TextWrapMode::Extend);
        let response = cell.add_enabled(button.state.is_ok(), widget);
        match &button.state {
            Ok(()) => {
                clicked = response.on_hover_text(&button.tip).clicked();
            }
            Err(why) => {
                response.on_disabled_hover_text(theme::with_code(ui, why, |t| t));
            }
        }
        right = at.left() - BADGE_GAP - 4.0;
    }
    for (n, badge) in row.badges.iter().enumerate().rev() {
        let text = badge_galley(ui, badge);
        let size = text.size() + 2.0 * BADGE_PAD;
        let at = Rect::from_min_size(Pos2::new(right - size.x, centre - size.y / 2.0), size);
        ui.painter()
            .rect_filled(at, 4.0, theme::badge_fill(badge.color, dark));
        ui.painter().galley(at.min + BADGE_PAD, text, badge.color);
        if !badge.tip.is_empty() {
            let tip_id = ui.scope_id().with((id, "badge", n));
            ui.interact(at, tip_id, Sense::hover())
                .on_hover_text(&badge.tip);
        }
        right = at.left() - BADGE_GAP;
    }
    clicked
}

/// How wide a row's badges and button are side by side, with the room between them.
fn right_width(ui: &egui::Ui, row: &Row) -> f32 {
    let badges: f32 = (row.badges.iter())
        .map(|b| badge_galley(ui, b).size().x + 2.0 * BADGE_PAD.x + BADGE_GAP)
        .sum();
    let button =
        (row.button.as_ref()).map_or(0.0, |b| button_size(ui, b.label).x + BADGE_GAP + 4.0);
    (badges + button - BADGE_GAP).max(0.0)
}

/// A button's size: its label and the button's padding, a control's height.
fn button_size(ui: &egui::Ui, label: &str) -> Vec2 {
    let font = FontId::proportional(theme::BODY);
    let text = (ui.painter()).layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let pad = ui.spacing().button_padding.x;
    Vec2::new(
        text.size().x + 2.0 * pad + 2.0,
        ui.spacing().interact_size.y,
    )
}

fn head_galley(ui: &egui::Ui, title: &str) -> Arc<Galley> {
    let font = FontId::proportional(theme::SMALL);
    (ui.painter()).layout_no_wrap(title.to_string(), font, Color32::PLACEHOLDER)
}

fn badge_galley(ui: &egui::Ui, badge: &Badge) -> Arc<Galley> {
    let font = FontId::proportional(theme::SMALL);
    (ui.painter()).layout_no_wrap(badge.text.clone(), font, Color32::PLACEHOLDER)
}

fn cell_galley(ui: &egui::Ui, cell: &Cell, wrap: Option<f32>) -> Arc<Galley> {
    galley(ui, &cell.text, cell.mono, wrap)
}

/// `text` laid out in the regular face, or the monospace one for an exact name, wrapped at
/// `wrap` — in egui's placeholder colour, so that the painter's colour is the one drawn.
fn galley(ui: &egui::Ui, text: &str, mono: bool, wrap: Option<f32>) -> Arc<Galley> {
    let font = match mono {
        true => FontId::monospace(13.0),
        false => FontId::proportional(theme::BODY),
    };
    let job = egui::text::LayoutJob::simple(
        text.to_string(),
        font,
        Color32::PLACEHOLDER,
        wrap.unwrap_or(f32::INFINITY),
    );
    ui.painter().layout_job(job)
}

//! The create wizard's controls: option tiles, check items, input boxes, a segmented choice, folds,
//! badges and banners. Each is painted from core's form, takes no keyboard, reports its role to the
//! accessibility tree, and shows core's cursor as a ring outside it.

use aisbx_core::app::TextInput;
use eframe::egui::{
    self, text::LayoutJob, text::TextWrapping, Color32, FontId, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, TextFormat, Vec2,
};

use crate::theme;

/// The corner radius of every control.
pub const RADIUS: f32 = 6.0;

/// Room inside a control, left and right.
const PAD: f32 = 12.0;

/// What a control is drawn with on this frame's visuals.
struct Ink {
    dark: bool,
    text: Color32,
    strong: Color32,
    weak: Color32,
    border: Color32,
    hover: Color32,
    field: Color32,
    accent: Color32,
}

impl Ink {
    fn of(ui: &egui::Ui) -> Self {
        let v = ui.visuals();
        Self {
            dark: v.dark_mode,
            text: v.text_color(),
            strong: v.strong_text_color(),
            weak: v.weak_text_color(),
            border: v.widgets.noninteractive.bg_stroke.color,
            hover: v.widgets.hovered.bg_stroke.color,
            field: v.extreme_bg_color,
            accent: theme::accent(v.dark_mode),
        }
    }
}

/// Mark core's cursor on `rect`: a ring in the accent, just outside it.
pub fn focus_ring(ui: &egui::Ui, rect: Rect) {
    let accent = theme::accent(ui.visuals().dark_mode);
    ui.painter().rect_stroke(
        rect.expand(3.0),
        RADIUS + 3.0,
        Stroke::new(2.0, accent),
        StrokeKind::Outside,
    );
}

/// Text laid out to `width`, cut to `rows` lines with `…`.
fn text(
    ui: &egui::Ui,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
    rows: usize,
) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::single_section(
        theme::drawable(text).into_owned(),
        TextFormat::simple(FontId::proportional(size), color),
    );
    job.wrap = TextWrapping {
        max_width: width.max(1.0),
        max_rows: rows,
        break_anywhere: false,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// A radio's mark: a ring, with a dot inside while it is on.
fn radio_mark(ui: &egui::Ui, center: Pos2, on: bool, color: Color32) {
    let painter = ui.painter();
    painter.circle_stroke(center, 8.0, Stroke::new(1.5, color));
    if on {
        painter.circle_filled(center, 4.5, color);
    }
}

/// A tick about 9 points wide, centred on `center`: drawn rather than written, since the
/// window's fonts have no ✓.
pub fn tick(painter: &egui::Painter, center: Pos2, color: Color32) {
    let mark = Stroke::new(2.0, color);
    let (a, b, d) = (
        center + Vec2::new(-4.5, 0.0),
        center + Vec2::new(-1.5, 3.0),
        center + Vec2::new(4.5, -3.5),
    );
    painter.line_segment([a, b], mark);
    painter.line_segment([b, d], mark);
}

/// A check box's mark: a rounded square, filled in `color` and ticked while it is on.
fn check_mark(ui: &egui::Ui, rect: Rect, on: bool, color: Color32, ink: &Ink) {
    let painter = ui.painter();
    match on {
        true => {
            painter.rect_filled(rect, 3.0, color);
            tick(painter, rect.center(), theme::on_accent(ink.dark));
        }
        false => {
            painter.rect_filled(rect, 3.0, ink.field);
            painter.rect_stroke(rect, 3.0, Stroke::new(1.5, color), StrokeKind::Inside);
        }
    }
}

/// A small pill of text: what a choice is (`recommended`, `only at creation`, `key missing`).
pub struct Badge<'a> {
    pub text: &'a str,
    pub color: Color32,
}

fn badge_galley(ui: &egui::Ui, badge: &Badge<'_>) -> std::sync::Arc<egui::Galley> {
    ui.painter().layout_no_wrap(
        badge.text.to_string(),
        FontId::proportional(11.5),
        badge.color,
    )
}

/// Paint `badges` right to left, ending at `right` on the line centred on `y`. Returns where the
/// leftmost starts.
fn paint_badges(ui: &egui::Ui, badges: &[Badge<'_>], right: f32, y: f32) -> f32 {
    let mut x = right;
    for badge in badges.iter().rev() {
        let galley = badge_galley(ui, badge);
        let size = galley.size() + Vec2::new(14.0, 4.0);
        let rect = Rect::from_min_size(Pos2::new(x - size.x, y - size.y / 2.0), size);
        let painter = ui.painter();
        painter.rect(
            rect,
            size.y / 2.0,
            badge.color.gamma_multiply(0.12),
            Stroke::new(1.0, badge.color.gamma_multiply(0.6)),
            StrokeKind::Inside,
        );
        painter.galley(rect.min + Vec2::new(7.0, 2.0), galley, badge.color);
        x = rect.left() - 6.0;
    }
    x
}

/// One choice among several, as a tile: the agent's colour down its left edge, a radio, the
/// title, a line saying what it is and one saying what it brings, badges on the title's line.
pub struct Tile<'a> {
    pub title: &'a str,
    pub blurb: &'a str,
    pub detail: &'a str,
    pub badges: Vec<Badge<'a>>,
    /// The agent's colour, for the edge and the radio.
    pub color: Color32,
    pub selected: bool,
    pub focused: bool,
}

/// How tall `tile` is drawn `width` wide: what a line of tiles takes the tallest of, so they
/// line up.
pub fn tile_height(ui: &egui::Ui, tile: &Tile<'_>, width: f32) -> f32 {
    tile_text(ui, tile, width).3
}

/// The tile's title, blurb and detail laid out, and the height they need.
#[allow(clippy::type_complexity)]
fn tile_text(
    ui: &egui::Ui,
    tile: &Tile<'_>,
    width: f32,
) -> (
    std::sync::Arc<egui::Galley>,
    Option<std::sync::Arc<egui::Galley>>,
    Option<std::sync::Arc<egui::Galley>>,
    f32,
) {
    let ink = Ink::of(ui);
    let inner = width - PAD * 2.0 - 28.0;
    let title = text(ui, tile.title, 15.5, ink.strong, inner, 1);
    let blurb = (!tile.blurb.is_empty()).then(|| text(ui, tile.blurb, 13.0, ink.text, inner, 2));
    let detail = (!tile.detail.is_empty()).then(|| text(ui, tile.detail, 12.5, ink.weak, inner, 2));
    let mut height = 12.0 + title.size().y + 10.0;
    for galley in [&blurb, &detail].into_iter().flatten() {
        height += galley.size().y + 2.0;
    }
    (title, blurb, detail, height)
}

/// Draw `tile` `width` wide and at least `height` tall. A click on it is the caller's to hand to
/// core.
pub fn tile(ui: &mut egui::Ui, tile: Tile<'_>, width: f32, height: f32) -> Response {
    let ink = Ink::of(ui);
    let (title, blurb, detail, needed) = tile_text(ui, &tile, width);
    let height = height.max(needed);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), Sense::CLICK);
    let hovered = response.hovered();
    let (fill, stroke) = match (tile.selected, hovered) {
        (true, _) => (
            ink.accent.gamma_multiply(0.10),
            Stroke::new(2.0, ink.accent),
        ),
        (false, true) => (ink.field, Stroke::new(1.0, ink.hover)),
        (false, false) => (ink.field, Stroke::new(1.0, ink.border)),
    };
    let painter = ui.painter();
    painter.rect(rect, RADIUS, fill, stroke, StrokeKind::Inside);
    let edge = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
    painter.rect_filled(
        edge.shrink2(Vec2::new(0.0, 6.0))
            .translate(Vec2::new(3.0, 0.0)),
        2.0,
        tile.color,
    );
    let mut y = rect.top() + 12.0;
    let x = rect.left() + PAD + 28.0;
    radio_mark(
        ui,
        Pos2::new(rect.left() + PAD + 10.0, y + title.size().y / 2.0),
        tile.selected,
        match tile.selected {
            true => ink.accent,
            false => tile.color,
        },
    );
    let line = y + title.size().y / 2.0;
    paint_badges(ui, &tile.badges, rect.right() - PAD, line);
    let painter = ui.painter();
    painter.galley(Pos2::new(x, y), title.clone(), ink.strong);
    y += title.size().y + 4.0;
    for galley in [blurb, detail].into_iter().flatten() {
        let size = galley.size().y;
        painter.galley(Pos2::new(x, y), galley, ink.text);
        y += size + 2.0;
    }
    if tile.focused {
        focus_ring(ui, rect);
    }
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::Role::RadioButton, true, tile.selected, tile.title)
    });
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// What an item's mark is: a box ticked on its own, or a radio of a group.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Check,
    Radio,
}

/// A choice that is on or not, as a list item: its mark, the title and badges on one line, a
/// muted detail under them. A disabled item is drawn muted and takes no click.
pub struct Item<'a> {
    pub mark: Mark,
    pub title: &'a str,
    pub detail: &'a str,
    pub badges: Vec<Badge<'a>>,
    /// The box's colour: the kit's, or the accent.
    pub color: Color32,
    pub checked: bool,
    pub enabled: bool,
    pub focused: bool,
}

/// Draw `item` `width` wide, its detail cut to two lines.
pub fn check_item(ui: &mut egui::Ui, item: Item<'_>, width: f32) -> Response {
    let ink = Ink::of(ui);
    let (title_color, detail_color) = match item.enabled {
        true => (ink.strong, ink.weak),
        false => (ink.weak, ink.weak),
    };
    let inner = width - 36.0 - 8.0;
    let badges: f32 = item
        .badges
        .iter()
        .map(|b| badge_galley(ui, b).size().x + 20.0)
        .sum();
    let title = text(ui, item.title, 14.5, title_color, inner - badges, 1);
    let detail =
        (!item.detail.is_empty()).then(|| text(ui, item.detail, 12.5, detail_color, inner, 2));
    let height = 8.0 + title.size().y + detail.as_ref().map_or(0.0, |d| d.size().y + 2.0) + 8.0;
    let sense = match item.enabled {
        true => Sense::CLICK,
        false => Sense::hover(),
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, height), sense);
    let painter = ui.painter();
    if response.hovered() && item.enabled {
        painter.rect_filled(rect, RADIUS, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let line = rect.top() + 8.0 + title.size().y / 2.0;
    let mark = Rect::from_center_size(Pos2::new(rect.left() + 18.0, line), Vec2::splat(16.0));
    let color = match item.enabled {
        true => item.color,
        false => ink.weak,
    };
    match item.mark {
        Mark::Check => check_mark(ui, mark, item.checked, color, &ink),
        Mark::Radio => radio_mark(ui, mark.center(), item.checked, color),
    }
    let x = rect.left() + 36.0;
    paint_badges(ui, &item.badges, rect.right() - 8.0, line);
    let painter = ui.painter();
    painter.galley(Pos2::new(x, rect.top() + 8.0), title, title_color);
    if let Some(detail) = detail {
        let y = rect.top() + 8.0 + 21.0;
        painter.galley(Pos2::new(x, y), detail, detail_color);
    }
    if item.focused {
        focus_ring(ui, rect);
    }
    let role = match item.mark {
        Mark::Check => egui::Role::CheckBox,
        Mark::Radio => egui::Role::RadioButton,
    };
    response
        .widget_info(|| egui::WidgetInfo::selected(role, item.enabled, item.checked, item.title));
    match item.enabled {
        true => response.on_hover_cursor(egui::CursorIcon::PointingHand),
        false => response,
    }
}

/// A line of text a field holds, as the window shows it: dots for a secret, one per character.
fn shown(input: &TextInput, masked: bool) -> String {
    crate::form::shown(input, masked)
}

/// An input box over one of core's text fields: the value (dots for a secret) or a muted
/// placeholder, and core's caret while its cursor is on it. Red-edged while `problem` holds.
pub struct Input<'a> {
    pub label: &'a str,
    pub input: &'a TextInput,
    pub masked: bool,
    pub placeholder: &'a str,
    pub focused: bool,
    pub problem: bool,
    pub monospace: bool,
    /// The value is core's suggestion, which the first character typed replaces: drawn muted.
    pub suggested: bool,
}

/// Draw `field` `width` wide. A click on it is the caller's: core moves its cursor there.
pub fn input_box(ui: &mut egui::Ui, field: Input<'_>, width: f32) -> Response {
    let ink = Ink::of(ui);
    let dark = ink.dark;
    let font = match field.monospace {
        true => FontId::monospace(13.5),
        false => FontId::proportional(14.5),
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::CLICK);
    let stroke = match (field.problem, response.hovered()) {
        (true, _) => Stroke::new(1.5, theme::danger(dark)),
        (false, true) => Stroke::new(1.0, ink.hover),
        (false, false) => Stroke::new(1.0, ink.border),
    };
    let painter = ui.painter();
    painter.rect(rect, RADIUS, ink.field, stroke, StrokeKind::Inside);
    let value = shown(field.input, field.masked);
    let clip = rect.shrink2(Vec2::new(10.0, 2.0));
    let painter = painter.with_clip_rect(clip);
    if value.is_empty() && !field.focused {
        let galley = painter.layout_no_wrap(field.placeholder.to_string(), font, ink.weak);
        let y = rect.center().y - galley.size().y / 2.0;
        painter.galley(Pos2::new(clip.left(), y), galley, ink.weak);
    } else {
        let chars: Vec<char> = value.chars().collect();
        let at = field.input.cursor.min(chars.len());
        let before: String = chars[..at].iter().collect();
        let color = match field.suggested {
            true => ink.weak,
            false => ink.text,
        };
        let galley = painter.layout_no_wrap(value.clone(), font.clone(), color);
        let caret = painter.layout_no_wrap(before, font, color).size().x;
        // Keep the caret in view: the text slides left once it runs past the box.
        let shift = (caret - clip.width() + 4.0).max(0.0);
        let y = rect.center().y - galley.size().y / 2.0;
        let origin = Pos2::new(clip.left() - shift, y);
        let height = galley.size().y;
        painter.galley(origin, galley, color);
        if field.focused {
            let x = origin.x + caret;
            let accent = theme::accent(dark);
            painter.line_segment(
                [Pos2::new(x, y + 1.0), Pos2::new(x, y + height - 1.0)],
                Stroke::new(1.5, accent),
            );
        }
    }
    if field.focused {
        focus_ring(ui, rect);
    }
    response.widget_info(|| {
        let mut info =
            egui::WidgetInfo::text_edit(true, value.clone(), value.clone(), field.placeholder);
        info.label = Some(field.label.to_string());
        info
    });
    response.on_hover_cursor(egui::CursorIcon::Text)
}

/// A labelled field: the label above, the control, then either what is wrong with its answer,
/// in the danger colour, or a muted line saying what goes in it.
pub fn labelled(
    ui: &mut egui::Ui,
    label: &str,
    gloss: &str,
    problem: Option<&str>,
    control: impl FnOnce(&mut egui::Ui) -> Response,
) -> Response {
    let ink = Ink::of(ui);
    ui.add_space(6.0);
    ui.label(theme::strong(egui::RichText::new(label).size(13.5)).color(ink.strong));
    ui.add_space(2.0);
    let response = control(ui);
    ui.add_space(2.0);
    match problem {
        Some(problem) => {
            let red = theme::danger(ink.dark);
            let problem = theme::with_code(ui, problem, |t| t.size(12.5).color(red));
            ui.add(egui::Label::new(problem).wrap());
        }
        None if !gloss.is_empty() => {
            let note = theme::with_code(ui, gloss, |t| t.size(12.5).color(ink.weak));
            ui.add(egui::Label::new(note).wrap());
        }
        None => {}
    }
    response
}

/// One of several named values, as a segmented control. Returns the control's response and the
/// segment clicked, if one was.
pub fn segmented(
    ui: &mut egui::Ui,
    label: &str,
    options: &[String],
    chosen: Option<usize>,
    focused: bool,
) -> (Response, Option<usize>) {
    let ink = Ink::of(ui);
    let font = FontId::proportional(14.0);
    let galleys: Vec<_> = options
        .iter()
        .map(|o| (ui.painter()).layout_no_wrap(o.clone(), font.clone(), Color32::PLACEHOLDER))
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 28.0).collect();
    let total: f32 = widths.iter().sum();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(total, 32.0), Sense::hover());
    let painter = ui.painter();
    painter.rect(
        rect,
        RADIUS,
        ink.field,
        Stroke::new(1.0, ink.border),
        StrokeKind::Inside,
    );
    let mut clicked = None;
    let mut x = rect.left();
    for (n, (galley, w)) in galleys.into_iter().zip(&widths).enumerate() {
        let segment = Rect::from_min_size(Pos2::new(x, rect.top()), Vec2::new(*w, rect.height()));
        let id = ui.scope_id().with(("segment", label, n));
        let hit = ui.interact(segment, id, Sense::CLICK);
        let on = chosen == Some(n);
        let color = match on {
            true => {
                ui.painter()
                    .rect_filled(segment.shrink(2.0), RADIUS - 2.0, ink.accent);
                theme::on_accent(ink.dark)
            }
            false if hit.hovered() => {
                let tint = ui.visuals().widgets.hovered.weak_bg_fill;
                ui.painter()
                    .rect_filled(segment.shrink(2.0), RADIUS - 2.0, tint);
                ink.text
            }
            false => ink.text,
        };
        let at = segment.center() - galley.size() / 2.0;
        ui.painter().galley(at, galley, color);
        if n > 0 && !on && chosen != Some(n - 1) {
            ui.painter().vline(
                x,
                segment.y_range().shrink(8.0),
                Stroke::new(1.0, ink.border),
            );
        }
        let name = options[n].clone();
        hit.widget_info(|| egui::WidgetInfo::selected(egui::Role::RadioButton, true, on, &name));
        if hit
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            clicked = Some(n);
        }
        x += w;
    }
    if focused {
        focus_ring(ui, rect);
    }
    (response, clicked)
}

/// How much a banner matters.
#[derive(Clone, Copy)]
pub enum Tone {
    Warn,
    Danger,
}

/// A note that matters, framed, with a bar in its colour down the left edge.
pub fn banner(ui: &mut egui::Ui, tone: Tone, text: &str) {
    let dark = ui.visuals().dark_mode;
    let color = match tone {
        Tone::Warn => theme::warn(dark),
        Tone::Danger => theme::danger(dark),
    };
    let frame = egui::Frame::new()
        .fill(color.gamma_multiply(0.10))
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.45)))
        .corner_radius(RADIUS)
        .inner_margin(egui::Margin {
            left: 14,
            right: 10,
            top: 8,
            bottom: 8,
        });
    let response = frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        let color = ui.visuals().text_color();
        ui.add(egui::Label::new(theme::with_code(ui, text, |t| t.color(color))).wrap());
    });
    let rect = response.response.rect;
    let bar = Rect::from_min_size(rect.min, Vec2::new(4.0, rect.height()));
    ui.painter().rect_filled(bar, 2.0, color);
}

/// A fold of a step: a row that shows or hides the answers under it, which are all optional.
pub struct Fold<'a> {
    pub title: &'a str,
    /// What is set under it, drawn in the accent; `None` while nothing is.
    pub summary: Option<&'a str>,
    /// What it holds, drawn while nothing under it is set.
    pub gloss: &'a str,
    pub open: bool,
    pub focused: bool,
}

/// The word after a fold's title.
pub const OPTIONAL: &str = "Optional";

/// Draw `fold` `width` wide: its triangle (open ▾, folded ▸, painted), the title, *Optional*,
/// then its summary or gloss on the same line, cut with `…`.
pub fn fold(ui: &mut egui::Ui, fold: Fold<'_>, width: f32) -> Response {
    let ink = Ink::of(ui);
    ui.add_space(10.0);
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::CLICK);
    let painter = ui.painter();
    if response.hovered() {
        painter.rect_filled(rect, RADIUS, ui.visuals().widgets.hovered.weak_bg_fill);
    }
    let tip = Pos2::new(rect.left() + 14.0, rect.center().y);
    let triangle = match fold.open {
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
    painter.add(egui::Shape::convex_polygon(
        triangle,
        ink.text,
        Stroke::NONE,
    ));
    let font = FontId::new(15.5, theme::strong_family());
    let title = painter.layout_no_wrap(fold.title.to_string(), font, ink.strong);
    let mut x = rect.left() + 30.0;
    let y = |g: &egui::Galley| rect.center().y - g.size().y / 2.0;
    let title_y = y(&title);
    let title_w = title.size().x;
    painter.galley(Pos2::new(x, title_y), title, ink.strong);
    x += title_w + 10.0;
    let optional = painter.layout_no_wrap(OPTIONAL.into(), FontId::proportional(12.5), ink.weak);
    let optional_y = y(&optional);
    let optional_w = optional.size().x;
    painter.galley(Pos2::new(x, optional_y), optional, ink.weak);
    x += optional_w + 14.0;
    let (rest, color) = match fold.summary {
        Some(summary) => (summary, ink.accent),
        None => (fold.gloss, ink.weak),
    };
    if !rest.is_empty() {
        let room = rect.right() - PAD - x;
        let galley = text(ui, rest, 12.5, color, room, 1);
        let at = Pos2::new(x, y(&galley));
        ui.painter().galley(at, galley, color);
    }
    if fold.focused {
        focus_ring(ui, rect);
    }
    let label = fold.title.to_string();
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, fold.open, &label));
    response.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// A section of a step: its title, and under it what the section is for.
pub fn section(ui: &mut egui::Ui, title: &str, gloss: &str) {
    ui.add_space(14.0);
    ui.label(theme::strong(egui::RichText::new(title).size(15.5)));
    if !gloss.is_empty() {
        let weak = ui.visuals().weak_text_color();
        ui.label(egui::RichText::new(gloss).size(12.5).color(weak));
    }
    ui.add_space(4.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What drawing `f` told the accessibility tree, as text.
    fn told(f: impl Fn(&mut egui::Ui)) -> String {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut tree = String::new();
        for _ in 0..2 {
            let mut output = ctx.run_ui(egui::RawInput::default(), |ui| f(ui));
            output.textures_delta.clear();
            if let Some(update) = output.platform_output.accesskit_update {
                tree = format!("{update:?}");
            }
        }
        tree
    }

    /// A secret's field tells its dots, never its value.
    #[test]
    fn the_controls_tell_the_accessibility_tree_what_they_are() {
        let input = TextInput::new("hunter2");
        let tree = told(|ui| {
            let field = Input {
                label: "token",
                input: &input,
                masked: true,
                placeholder: "paste it",
                focused: true,
                problem: false,
                monospace: false,
                suggested: false,
            };
            input_box(ui, field, 200.0);
        });
        assert!(tree.contains("•••••••"), "{tree}");
        assert!(!tree.contains("hunter2"), "{tree}");

        let tree = told(|ui| {
            let item = Item {
                mark: Mark::Check,
                title: "rtk",
                detail: "a kit",
                badges: Vec::new(),
                color: Color32::RED,
                checked: true,
                enabled: true,
                focused: false,
            };
            check_item(ui, item, 300.0);
            let tile = Tile {
                title: "Claude Code",
                blurb: "",
                detail: "",
                badges: Vec::new(),
                color: Color32::RED,
                selected: false,
                focused: true,
            };
            super::tile(ui, tile, 300.0, 0.0);
        });
        assert!(tree.contains("CheckBox") && tree.contains("rtk"), "{tree}");
        assert!(
            tree.contains("RadioButton") && tree.contains("Claude Code"),
            "{tree}"
        );
    }
}

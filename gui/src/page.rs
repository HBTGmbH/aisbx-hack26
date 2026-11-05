//! The frame of every page but the list and the create wizard: one centred column of readable
//! width under the page's title, and the page's buttons in a bar along the window's foot. Pages
//! that list things use its groups ([`group`]) of items ([`item`]), each with its own buttons.

use eframe::egui::{
    self, Color32, CursorIcon, FontId, Galley, Pos2, Rect, RichText, Role, Sense, Stroke, Vec2,
    WidgetInfo,
};
use std::sync::Arc;

use crate::form::{self, Look};
use crate::theme;

/// The column's widest, and the room the window keeps at either side of it.
pub const COLUMN: f32 = 760.0;
const SIDE: f32 = 24.0;

/// The page's bar along the window's foot: `add` in the column's width. Drawn before [`body`],
/// which takes the rest of the window.
pub fn bar(ui: &mut egui::Ui, id: &str, add: impl FnOnce(&mut egui::Ui)) {
    let frame =
        egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(0, 10));
    egui::Panel::bottom(format!("{id}-bar"))
        .frame(frame)
        .show(ui, |ui| column(ui, add));
}

/// The page above its [`bar`]: the title, what the page is for (`subtitle`, none when empty) and
/// `add`, scrolling in the column. `id` keeps the page's scroll offset its own.
pub fn body(
    ui: &mut egui::Ui,
    id: &str,
    title: &str,
    subtitle: &str,
    add: impl FnOnce(&mut egui::Ui),
) {
    let frame = egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::ZERO);
    egui::CentralPanel::default().frame(frame).show(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt((id, "page"))
            .auto_shrink(false)
            .show(ui, |ui| {
                column(ui, |ui| {
                    ui.add_space(22.0);
                    ui.label(theme::strong(
                        RichText::new(title).size(theme::HEADING + 1.0),
                    ));
                    if !subtitle.is_empty() {
                        ui.add_space(2.0);
                        paragraph(ui, subtitle, ui.visuals().weak_text_color());
                    }
                    ui.add_space(8.0);
                    add(ui);
                    ui.add_space(24.0);
                });
            });
    });
}

/// `add` in the column: at most [`COLUMN`] wide, centred, [`SIDE`] from the window's edges.
fn column(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let full = ui.available_width();
    let width = (full - 2.0 * SIDE).clamp(0.0, COLUMN);
    let left = ((full - width) / 2.0).max(0.0);
    let spacing = ui.spacing().item_spacing;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.add_space(left);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing = spacing;
            ui.set_width(width);
            add(ui);
        });
    });
}

/// Text wrapped at [`form::PARA_W`] or the column, whichever is narrower, in `color`, its
/// `code` pieces as code ([`theme::with_code`]).
pub fn paragraph(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width().min(form::PARA_W));
        let text = theme::with_code(ui, text, |t| t.color(color));
        ui.add(egui::Label::new(text).wrap());
    });
}

/// A group's heading: its title, and beside it, muted, what the group holds.
pub fn heading(ui: &mut egui::Ui, title: &str, gloss: &str) {
    heading_with(ui, title, gloss, |_| {});
}

/// [`heading`] with `trailing` after it on its line: a button that acts on the whole section.
pub fn heading_with(
    ui: &mut egui::Ui,
    title: &str,
    gloss: &str,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(16.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(theme::with_code(ui, title, |t| {
            theme::strong(t.size(theme::SECTION))
        }));
        if !gloss.is_empty() {
            ui.label(theme::with_code(ui, gloss, |t| {
                t.size(theme::SMALL + 0.5).weak()
            }));
        }
        trailing(ui);
    });
    ui.add_space(4.0);
}

/// Items on one raised fill inside a hairline, as a desktop window draws a list of settings.
pub fn group<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let dark = ui.visuals().dark_mode;
    egui::Frame::new()
        .fill(theme::raised(dark))
        .stroke(Stroke::new(1.0, theme::line(dark)))
        .corner_radius(8)
        .inner_margin(egui::Margin::symmetric(0, 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            add(ui)
        })
        .inner
}

/// One item of a [`group`].
pub struct Item<'a> {
    /// What the item is.
    pub title: &'a str,
    /// A glyph in a colour before the title: the flag of a held request.
    pub mark: Option<(&'a str, Color32)>,
    /// What it is or where it points, muted, after the title.
    pub meta: &'a str,
    /// Its state, before the buttons: a stored value as sbx masks it, what is missing.
    pub state: Option<State>,
    /// The buttons that act on it, as `(label, look)`; an inert one is drawn disabled.
    pub actions: &'a [(&'a str, Look)],
    /// The room kept for buttons at the right edge, at least its own: the widest of its group's
    /// ([`actions_width`]), so the states of a group's items end in one column.
    pub reserve: f32,
    /// Core's cursor is on it.
    pub selected: bool,
    /// A hairline above it: every item of a group but its first.
    pub rule: bool,
}

/// An item's state, as [`Item::state`] shows it.
pub struct State {
    pub text: String,
    pub color: Color32,
    /// A value sbx printed, such as a masked secret: in the monospace face.
    pub value: bool,
}

/// What a click on an item did this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Picked {
    /// The item itself was clicked: select it.
    pub item: bool,
    /// One of its buttons was, by its place in [`Item::actions`].
    pub action: Option<usize>,
}

/// An item's height, and the room at its sides and between its parts.
const ITEM_H: f32 = 40.0;
const PAD: f32 = 14.0;
const GAP: f32 = 12.0;

/// Draw `item` across the group, and under it, while it is selected, `detail`: what core says
/// about it. `follow`: scroll it into view (core's cursor has just moved onto it).
pub fn item(
    ui: &mut egui::Ui,
    item: Item<'_>,
    follow: bool,
    detail: impl FnOnce(&mut egui::Ui),
) -> Picked {
    let dark = ui.visuals().dark_mode;
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ITEM_H), Sense::CLICK);
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::selected(Role::ListItem, true, item.selected, item.title));
    if follow {
        response.scroll_to_me(None);
    }
    if item.rule {
        let y = rect.top() + 0.5;
        let line = Stroke::new(1.0, theme::line(dark));
        ui.painter()
            .hline(rect.left() + PAD..=rect.right() - PAD, y, line);
    }
    // Reserved under everything the item draws, set after its notes, so the band covers them too.
    let band = ui.painter().add(egui::Shape::Noop);
    let strip = ui.painter().add(egui::Shape::Noop);

    let mut right = rect.right() - PAD;
    let mut picked = Picked::default();
    for (n, (label, look)) in item.actions.iter().enumerate().rev() {
        let size = button_size(ui, label);
        let at = Rect::from_min_size(
            Pos2::new(right - size.x, rect.center().y - size.y / 2.0),
            size,
        );
        // In a child UI of its own, which moves nothing after it, and as wide as its slot, so the
        // right edges of the items' buttons line up.
        let enabled = *look != Look::Inert;
        let mut cell = ui.new_child(egui::UiBuilder::new().max_rect(at));
        let clicked = cell
            .add_enabled(enabled, button(label, *look, dark).min_size(size))
            .clicked();
        if clicked {
            picked.action = Some(n);
        }
        right = at.left() - 6.0;
    }
    let buttons = actions_width(ui, item.actions).max(item.reserve);
    if buttons > 0.0 {
        right = rect.right() - PAD - buttons - GAP;
    }
    if let Some(state) = &item.state {
        let font = match state.value {
            true => FontId::monospace(13.0),
            false => FontId::proportional(theme::SMALL + 1.0),
        };
        let galley = one_line(ui, state.text.clone(), font, state.color, width * 0.4);
        let pos = Pos2::new(
            right - galley.size().x,
            rect.center().y - galley.size().y / 2.0,
        );
        right = pos.x - GAP;
        ui.painter().galley(pos, galley, state.color);
    }
    let mut x = rect.left() + PAD + 6.0;
    if let Some((glyph, color)) = item.mark {
        let galley = one_line(ui, glyph.into(), FontId::proportional(14.0), color, 20.0);
        let pos = Pos2::new(x, rect.center().y - galley.size().y / 2.0);
        x += galley.size().x + 6.0;
        ui.painter().galley(pos, galley, color);
    }
    let strong = ui.visuals().strong_text_color();
    let title = one_line(
        ui,
        item.title.into(),
        FontId::proportional(theme::BODY),
        strong,
        (right - x).max(0.0),
    );
    let title_w = title.size().x;
    let pos = Pos2::new(x, rect.center().y - title.size().y / 2.0);
    ui.painter().galley(pos, title, strong);
    let room = right - (x + title_w + 10.0);
    if !item.meta.is_empty() && room > 24.0 {
        let weak = ui.visuals().weak_text_color();
        let font = FontId::proportional(theme::SMALL + 0.5);
        let meta = one_line(ui, item.meta.into(), font, weak, room);
        let pos = Pos2::new(x + title_w + 10.0, rect.center().y - meta.size().y / 2.0);
        ui.painter().galley(pos, meta, weak);
    }
    picked.item = response.clicked() && picked.action.is_none();

    let mut whole = rect;
    if item.selected {
        let notes = egui::Frame::new()
            .inner_margin(egui::Margin {
                left: (PAD + 6.0) as i8,
                right: PAD as i8,
                top: 0,
                bottom: 12,
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 6.0;
                detail(ui);
            });
        whole = whole.union(notes.response.rect);
    }
    let fill = whole.shrink2(Vec2::new(4.0, 2.0));
    if item.selected {
        let bar = Rect::from_min_size(fill.min, Vec2::new(3.0, fill.height()))
            .shrink2(Vec2::new(0.0, 6.0));
        let painter = ui.painter();
        painter.set(band, egui::Shape::rect_filled(fill, 6.0, theme::band(dark)));
        painter.set(
            strip,
            egui::Shape::rect_filled(bar, 1.5, theme::accent(dark)),
        );
    } else if response.hovered() {
        let hover = ui.visuals().faint_bg_color;
        ui.painter()
            .set(band, egui::Shape::rect_filled(fill, 6.0, hover));
    }
    picked
}

/// One line of text, cut with `…` at `width`, its `code` pieces as code ([`theme::code_job`]).
pub fn one_line(
    ui: &egui::Ui,
    text: String,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<Galley> {
    let mut job = theme::code_job(ui, &text, font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    ui.painter().layout_job(job)
}

/// An item's button: clickable, never focusable — the page's keys are core's.
pub fn button(label: &str, look: Look, dark: bool) -> egui::Button<'static> {
    let text = RichText::new(label.to_string());
    let button = match look {
        Look::Primary => egui::Button::new(theme::strong(text).color(theme::on_accent(dark)))
            .fill(theme::accent(dark)),
        Look::Danger => egui::Button::new(text.color(theme::danger(dark))),
        Look::Plain | Look::Inert => egui::Button::new(text),
    };
    // An outline, so a button keeps its edge on the selection's band as on the group's fill.
    let edge = match look {
        Look::Primary => Stroke::NONE,
        Look::Plain | Look::Danger | Look::Inert => Stroke::new(1.0, theme::line(dark)),
    };
    button
        .stroke(edge)
        .wrap_mode(egui::TextWrapMode::Extend)
        .sense(Sense::CLICK)
}

/// How wide an item's buttons are, side by side: what [`item`] keeps for them at its right edge.
/// The widest of a group's is every item's [`Item::reserve`].
pub fn actions_width(ui: &egui::Ui, actions: &[(&str, Look)]) -> f32 {
    let widths = actions.iter().map(|(label, _)| button_size(ui, label).x);
    let count = actions.len() as f32;
    widths.sum::<f32>() + 6.0 * (count - 1.0).max(0.0)
}

/// How big an item's button is drawn: its label and the button's padding, 26 points high.
pub fn button_size(ui: &egui::Ui, label: &str) -> Vec2 {
    let font = FontId::proportional(theme::BODY);
    let text = ui
        .painter()
        .layout_no_wrap(label.to_string(), font, Color32::WHITE);
    let pad = ui.spacing().button_padding.x;
    // A little over the text and its padding, so rounding never makes egui wrap the label.
    Vec2::new((text.size().x + 2.0 * pad + 4.0).max(64.0), 26.0)
}

/// What core says about the selected item, wrapped as a paragraph in the text colour.
pub fn notes(ui: &mut egui::Ui, text: &str) {
    paragraph(ui, text, ui.visuals().text_color());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Draw one group of `items` in a 900-point window and hand back what each frame picked.
    fn frame(ctx: &egui::Context, items: &[Item<'_>], events: Vec<egui::Event>) -> Vec<Picked> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 400.0))),
            events,
            ..Default::default()
        };
        let mut picked = Vec::new();
        let mut output = ctx.run_ui(input, |ui| {
            group(ui, |ui| {
                for it in items {
                    let it = Item {
                        title: it.title,
                        mark: it.mark,
                        meta: it.meta,
                        state: None,
                        actions: it.actions,
                        reserve: it.reserve,
                        selected: it.selected,
                        rule: it.rule,
                    };
                    picked.push(item(ui, it, false, |ui| notes(ui, "what it is")));
                }
            });
        });
        output.textures_delta.clear();
        picked
    }

    fn click(ctx: &egui::Context, items: &[Item<'_>], at: Pos2) -> Vec<Picked> {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(
            ctx,
            items,
            vec![egui::Event::PointerMoved(at), button(true)],
        );
        frame(ctx, items, vec![button(false)])
    }

    #[test]
    fn an_items_line_draws_code_as_code() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            group(ui, |ui| {
                let it = Item {
                    title: "Cursor",
                    mark: None,
                    meta: "opens `~/proj` over SSH",
                    state: Some(State {
                        text: "`cursor` command not found".into(),
                        color: Color32::RED,
                        value: false,
                    }),
                    actions: &[],
                    reserve: 0.0,
                    selected: false,
                    rule: false,
                };
                item(ui, it, false, |_| {});
            });
        });
        output.textures_delta.clear();
        let (mut texts, mut mono) = (Vec::new(), Vec::new());
        for clipped in &output.shapes {
            if let egui::Shape::Text(text) = &clipped.shape {
                let job = &text.galley.job;
                texts.push(job.text.clone());
                for s in &job.sections {
                    if s.format.font_id.family == egui::FontFamily::Monospace {
                        mono.push(job.text[s.byte_range.start.0..s.byte_range.end.0].to_string());
                    }
                }
            }
        }
        assert!(texts.iter().all(|t| !t.contains('`')), "{texts:?}");
        assert_eq!(mono, ["cursor", "~/proj"], "{texts:?}");
    }

    #[test]
    fn a_groups_states_end_in_one_column_and_its_buttons_line_up() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let wide = [("Replace", Look::Plain), ("Delete", Look::Danger)];
        let short = [("Edit", Look::Plain), ("Delete", Look::Danger)];
        let alone = [("Delete the key", Look::Danger)];
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900.0, 400.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            group(ui, |ui| {
                let all: [&[(&str, Look)]; 3] = [&wide, &short, &alone];
                let reserve = (all.iter())
                    .map(|a| actions_width(ui, a))
                    .fold(0.0, f32::max);
                for (n, actions) in all.into_iter().enumerate() {
                    let it = Item {
                        title: "token",
                        mark: None,
                        meta: "",
                        state: Some(State {
                            text: format!("sk-{}*****", "x".repeat(n * 3)),
                            color: Color32::GREEN,
                            value: true,
                        }),
                        actions,
                        reserve,
                        selected: false,
                        rule: n > 0,
                    };
                    item(ui, it, false, |_| {});
                }
            });
        });
        output.textures_delta.clear();
        let mut texts: Vec<(String, Rect)> = Vec::new();
        let mut fills: Vec<Rect> = Vec::new();
        let mut collect = |shape: &egui::Shape| match shape {
            egui::Shape::Text(t) => texts.push((
                t.galley.text().to_string(),
                t.galley.rect.translate(t.pos.to_vec2()),
            )),
            egui::Shape::Rect(r) if r.fill.a() > 0 => fills.push(r.rect),
            _ => {}
        };
        for clipped in &output.shapes {
            match &clipped.shape {
                egui::Shape::Vec(shapes) => shapes.iter().for_each(&mut collect),
                shape => collect(shape),
            }
        }
        let rects = |prefix: &str| -> Vec<Rect> {
            (texts.iter())
                .filter(|(t, _)| t.starts_with(prefix))
                .map(|(_, r)| *r)
                .collect()
        };
        let states = rects("sk-");
        assert_eq!(states.len(), 3, "{texts:?}");
        assert!(
            states
                .iter()
                .all(|r| (r.right() - states[0].right()).abs() < 0.5),
            "the states end in one column: {states:?}"
        );
        // Each button's frame: the filled rectangle around its label.
        let frame_of = |label: &str| {
            let text = rects(label)[0];
            *(fills.iter())
                .filter(|f| f.contains(text.center()))
                .min_by(|a, b| a.area().total_cmp(&b.area()))
                .expect("a button's frame")
        };
        let (replace, edit) = (frame_of("Replace"), frame_of("Edit"));
        assert!(
            (replace.right() - edit.right()).abs() < 0.5,
            "{replace:?} {edit:?}"
        );
        let deletes = rects("Delete");
        assert!(deletes.len() == 3, "{texts:?}");
    }

    #[test]
    fn every_item_has_its_buttons_and_a_button_is_not_the_item() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx);
        let actions = [("Replace", Look::Plain), ("Delete", Look::Danger)];
        let items = [
            Item {
                title: "token private",
                mark: None,
                meta: "",
                state: None,
                actions: &actions,
                reserve: 0.0,
                selected: false,
                rule: false,
            },
            Item {
                title: "token work",
                mark: None,
                meta: "",
                state: None,
                actions: &actions,
                reserve: 0.0,
                selected: false,
                rule: true,
            },
        ];
        frame(&ctx, &items, Vec::new());
        // The second item sits right under the first; its Delete is at its right end.
        let second = 2.0 + ITEM_H;
        let row = |y: f32| Pos2::new(120.0, y);
        let delete = |y: f32| Pos2::new(900.0 - PAD - 30.0, y);
        let picked = click(&ctx, &items, row(2.0 + ITEM_H / 2.0));
        assert_eq!(
            picked[0],
            Picked {
                item: true,
                action: None
            }
        );
        let picked = click(&ctx, &items, delete(2.0 + ITEM_H / 2.0));
        assert_eq!(
            picked[0],
            Picked {
                item: false,
                action: Some(1)
            }
        );
        let picked = click(&ctx, &items, delete(second + ITEM_H / 2.0));
        assert_eq!(picked[1].action, Some(1), "{picked:?}");
        assert_eq!(picked[0], Picked::default());
    }
}

//! What the create wizard and every page share: the key conversion their keys go through, core's
//! cursor on their rows and buttons, and a page's line of buttons. No widget here takes the
//! keyboard, so egui's own Tab and Enter never act behind core's cursor.

use aisbx_core::app::{App, Hit, TextInput};
use aisbx_core::input::{KeyCode, KeyEvent, KeyModifiers};
use aisbx_core::models::{self, Model, Overrides, Slot};
use eframe::egui::{self, text::LayoutJob, Color32, FontId, RichText, TextFormat};

use crate::{controls, theme};

/// One egui event as the keys core's forms take, in order: text as characters, a key only when it
/// types nothing. Option- or Ctrl-Backspace sends Ctrl-W and Cmd-Backspace Ctrl-U, the platform's
/// word and line deletes. Cmd or Ctrl with A, E, K, U or W sends nothing: no shell chord edits a
/// field.
pub fn form_key(event: &egui::Event) -> Vec<KeyEvent> {
    match event {
        egui::Event::Text(text) | egui::Event::Paste(text) => text
            .chars()
            .filter(|c| !c.is_control())
            .map(|c| KeyEvent::from(KeyCode::Char(c)))
            .collect(),
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            let ctrl = modifiers.ctrl || modifiers.command;
            let code = match key {
                egui::Key::ArrowUp => KeyCode::Up,
                egui::Key::ArrowDown => KeyCode::Down,
                egui::Key::ArrowLeft => KeyCode::Left,
                egui::Key::ArrowRight => KeyCode::Right,
                egui::Key::Enter => KeyCode::Enter,
                egui::Key::Escape => KeyCode::Esc,
                egui::Key::Tab if modifiers.shift => KeyCode::BackTab,
                egui::Key::Tab => KeyCode::Tab,
                egui::Key::Backspace if modifiers.alt || (modifiers.ctrl && !modifiers.mac_cmd) => {
                    return vec![KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)];
                }
                egui::Key::Backspace if modifiers.mac_cmd => {
                    return vec![KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)];
                }
                egui::Key::Backspace => KeyCode::Backspace,
                egui::Key::Delete => KeyCode::Delete,
                egui::Key::Home => KeyCode::Home,
                egui::Key::End => KeyCode::End,
                egui::Key::PageUp => KeyCode::PageUp,
                egui::Key::PageDown => KeyCode::PageDown,
                other if ctrl => match letter(*other) {
                    Some('a' | 'e' | 'k' | 'u' | 'w') | None => return Vec::new(),
                    Some(c) => return vec![KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)],
                },
                _ => return Vec::new(),
            };
            let modifiers = match ctrl {
                true => KeyModifiers::CONTROL,
                false => KeyModifiers::NONE,
            };
            vec![KeyEvent::new(code, modifiers)]
        }
        _ => Vec::new(),
    }
}

/// The lower-case letter a key is, if it is one.
fn letter(key: egui::Key) -> Option<char> {
    let mut name = key.name().chars();
    match (name.next(), name.next()) {
        (Some(c), None) if c.is_ascii_alphabetic() => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// The rows of one form as drawn this frame: which one has core's cursor, and which was clicked.
pub struct Rows {
    focus: usize,
    /// The cursor moved since the last frame, so its row is scrolled into view.
    scroll: bool,
    pub clicked: Option<Hit>,
}

impl Rows {
    /// `id`: the form's, where the cursor of the last frame is kept.
    pub fn new(ui: &egui::Ui, id: &str, focus: usize) -> Self {
        let id = egui::Id::unique(("aisbx-form-focus", id));
        let scroll = ui.data(|d| d.get_temp::<usize>(id)) != Some(focus);
        ui.data_mut(|d| d.insert_temp(id, focus));
        Self {
            focus,
            scroll,
            clicked: None,
        }
    }

    /// Is core's cursor on row `index`?
    pub fn is_focused(&self, index: usize) -> bool {
        index == self.focus
    }

    /// Has core's cursor just moved onto row `index`, so it is to be scrolled into view?
    pub fn follows(&self, index: usize) -> bool {
        self.is_focused(index) && self.scroll
    }

    /// [`Rows::is_focused`] for a row that may not be one of the form's.
    pub fn focused(&self, index: Option<usize>) -> bool {
        index.is_some_and(|i| self.is_focused(i))
    }

    /// What a control drawn for row `index` leaves to the rows: scrolled into view when the
    /// cursor has just moved onto it, its click handed to core as a click on the row.
    pub fn take(&mut self, response: &egui::Response, index: Option<usize>) {
        if self.focused(index) && self.scroll {
            response.scroll_to_me(None);
        }
        if let (true, Some(i)) = (response.clicked(), index) {
            self.clicked = Some(Hit::FormRow(i));
        }
    }

    /// A click on one value of a choice row, for core.
    pub fn choose(&mut self, index: Option<usize>, value: usize) {
        if let Some(i) = index {
            self.clicked = Some(Hit::FormChoice(i, value));
        }
    }

    /// One row: clickable, never focusable, highlighted while core's cursor is on it. `index` is
    /// the row's place in the form's rows (`None`: drawn only, not a row of the form).
    pub fn row(
        &mut self,
        ui: &mut egui::Ui,
        index: Option<usize>,
        text: impl Into<egui::WidgetText>,
    ) -> egui::Response {
        let focused = index.is_some_and(|i| i == self.focus);
        // egui hides the frame of a button that is not hovered, a selected one's too, but that
        // frame is the only mark of core's cursor: the focused row keeps it.
        let response = ui.add(
            egui::Button::new(text)
                .selected(focused)
                .frame_when_inactive(focused)
                .wrap()
                .sense(egui::Sense::CLICK),
        );
        if focused && self.scroll {
            response.scroll_to_me(None);
        }
        if let (true, Some(i)) = (response.clicked(), index) {
            self.clicked = Some(Hit::FormRow(i));
        }
        response
    }

    /// A form's line of buttons, one per `(label, index, danger)` in core's order (the main action
    /// first, the way out last), drawn as a desktop window does: the way out at the left edge, the
    /// main action rightmost. That is core's order backwards, so Left and Right swap on the line
    /// ([`along_buttons`]).
    pub fn buttons(&mut self, ui: &mut egui::Ui, buttons: &[(&str, Option<usize>, bool)]) {
        let look = |n: usize, danger: bool| match (danger, n == 0 && buttons.len() > 1) {
            (true, _) => Look::Danger,
            (false, true) => Look::Primary,
            (false, false) => Look::Plain,
        };
        let (way_out, rest) = match buttons.split_last() {
            Some((last, rest)) if !rest.is_empty() => (Some(last), rest),
            _ => (None, buttons),
        };
        ui.horizontal(|ui| {
            if let Some((label, index, danger)) = way_out {
                self.button(ui, label, *index, look(buttons.len() - 1, *danger));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                for (n, (label, index, danger)) in rest.iter().enumerate() {
                    self.button(ui, label, *index, look(n, *danger));
                }
            });
        });
    }

    /// One button of a form's last line: clickable, never focusable, with core's cursor drawn as a
    /// ring outside it, as on the wizard's controls.
    pub fn button(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        index: Option<usize>,
        look: Look,
    ) -> egui::Response {
        let dark = ui.visuals().dark_mode;
        let focused = index.is_some_and(|i| i == self.focus);
        let text = theme::strong(RichText::new(label));
        let button = match look {
            Look::Primary => {
                egui::Button::new(text.color(theme::on_accent(dark))).fill(theme::accent(dark))
            }
            Look::Danger => egui::Button::new(text.color(theme::danger(dark))),
            Look::Plain | Look::Inert => egui::Button::new(text),
        };
        let button = button
            .min_size(egui::vec2(72.0, 26.0))
            .sense(egui::Sense::CLICK);
        let response = ui.add_enabled(look != Look::Inert, button);
        if focused {
            controls::focus_ring(ui, response.rect);
        }
        if focused && self.scroll {
            response.scroll_to_me(None);
        }
        if let (true, Some(i)) = (response.clicked(), index) {
            self.clicked = Some(Hit::FormRow(i));
        }
        response
    }
}

/// A key for core, as the window's layout means it: on a form's line of buttons, which the
/// window draws in core's order backwards ([`Rows::buttons`]), ← and → swap, so each moves the
/// way it points.
pub fn along_buttons(app: &App, key: KeyEvent) -> KeyEvent {
    let code = match key.code {
        KeyCode::Left if app.on_form_buttons() => KeyCode::Right,
        KeyCode::Right if app.on_form_buttons() => KeyCode::Left,
        code => code,
    };
    KeyEvent { code, ..key }
}

/// How a button on a form's last line looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// The form's main action, first on the line: filled in the accent, as the sandbox page's
    /// *Start* is.
    Primary,
    Plain,
    /// An action that costs something (a rebuild): in the danger colour, primary or not.
    Danger,
    /// In its place but doing nothing, like Back on the wizard's first step: drawn disabled.
    Inert,
}

/// Where a row's index is in `rows`, if it is one of them.
pub fn index_of<R: PartialEq>(rows: &[R], row: R) -> Option<usize> {
    rows.iter().position(|r| *r == row)
}

/// What a field shows for its value: one `•` per character when it is a secret — the TUI's rule —
/// so nothing of the value is ever laid out.
pub fn shown(input: &TextInput, masked: bool) -> String {
    match masked {
        true => "•".repeat(input.value.chars().count()),
        false => input.value.clone(),
    }
}

/// Two-part text: the main part, then a muted hint.
pub fn with_hint(ui: &egui::Ui, main: &str, color: Option<Color32>, hint: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_hint(ui, &mut job, main, color, hint);
    job
}

/// [`with_hint`], onto an existing line.
pub fn append_hint(
    ui: &egui::Ui,
    job: &mut LayoutJob,
    main: &str,
    color: Option<Color32>,
    hint: &str,
) {
    let font = FontId::proportional(14.0);
    let main_color = color.unwrap_or(ui.visuals().text_color());
    job.append(main, 0.0, TextFormat::simple(font.clone(), main_color));
    if !hint.is_empty() {
        let weak = TextFormat::simple(FontId::proportional(12.5), ui.visuals().weak_text_color());
        job.append(&format!("   {hint}"), 0.0, weak);
    }
}

/// How wide a paragraph runs at most: about 90 characters of the small face, a line the eye can
/// follow back.
pub const PARA_W: f32 = 620.0;

/// A paragraph of explanation, muted and wrapped at [`PARA_W`].
pub fn para(ui: &mut egui::Ui, text: &str) {
    ui.scope(|ui| {
        ui.set_max_width(ui.available_width().min(PARA_W));
        ui.add(egui::Label::new(RichText::new(text).small().weak()).wrap());
    });
}

/// A warning, in the warning colour.
pub fn warn(ui: &mut egui::Ui, text: &str) {
    let color = theme::warn(ui.visuals().dark_mode);
    ui.add(egui::Label::new(theme::with_code(ui, text, |t| t.color(color))).wrap());
}

/// A section heading.
pub fn section(ui: &mut egui::Ui, title: &str, gloss: &str) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(theme::strong(RichText::new(title)));
        if !gloss.is_empty() {
            ui.label(RichText::new(gloss).small().weak());
        }
    });
}

/// The three model aliases, with the open one's list under it — the create wizard's and the kits
/// editor's, with only the row type differing. `index` gives a row's place in the form.
#[allow(clippy::too_many_arguments)]
pub fn model_section<R: Copy>(
    ui: &mut egui::Ui,
    rows: &mut Rows,
    app: &App,
    over: &Overrides,
    open: Option<Slot>,
    index: &dyn Fn(R) -> Option<usize>,
    alias: fn(Slot) -> R,
    default: fn(Slot) -> R,
    pick: fn(Slot, usize) -> R,
) {
    let catalog = &app.catalog;
    let source = catalog.models_source.label();
    section(
        ui,
        &format!("Claude models · {source}"),
        "optional — this gateway may not serve Claude Code's own defaults",
    );
    for slot in Slot::ALL {
        let opened = open == Some(slot);
        let pinned = pinned(&catalog.models, over.get(slot));
        let text = format!(
            "{} {:<8} {pinned}",
            if opened { "▾" } else { "▸" },
            slot.alias()
        );
        rows.row(ui, index(alias(slot)), RichText::new(text).monospace())
            .on_hover_text(slot.hint());
        if !opened {
            continue;
        }
        ui.indent(("models", slot.alias()), |ui| {
            let mark = |on: bool| if on { "◉" } else { "○" };
            let none = over.get(slot).is_none();
            let text = with_hint(ui, &format!("{} {}", mark(none), slot.unset()), None, "");
            rows.row(ui, index(default(slot)), text);
            for (i, model) in catalog.models.iter().enumerate() {
                let on = over.get(slot) == Some(model.id.as_str());
                let text = with_hint(ui, &format!("{} {}", mark(on), model.name), None, &model.id);
                rows.row(ui, index(pick(slot, i)), text);
            }
        });
    }
}

/// What an alias is pinned to, by the model's name where the catalog has it.
fn pinned(models: &[Model], id: Option<&str>) -> String {
    match id {
        None => "(default)".into(),
        Some(id) => models::display_name(models, id).unwrap_or_else(|| id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    #[test]
    fn text_comes_as_characters_and_letters_only_as_text() {
        let codes = |e: &egui::Event| form_key(e).into_iter().map(|k| k.code).collect::<Vec<_>>();
        let text = egui::Event::Text("a b\u{7}".into());
        let want = [KeyCode::Char('a'), KeyCode::Char(' '), KeyCode::Char('b')];
        assert_eq!(codes(&text), want);
        assert_eq!(codes(&egui::Event::Paste("xy".into())).len(), 2);
        let none = egui::Modifiers::NONE;
        assert!(
            codes(&key(egui::Key::A, none)).is_empty(),
            "a letter as text only"
        );
        assert!(
            codes(&key(egui::Key::Space, none)).is_empty(),
            "Space as text only"
        );
        assert_eq!(codes(&key(egui::Key::Enter, none)), [KeyCode::Enter]);
        assert_eq!(
            codes(&key(egui::Key::Tab, egui::Modifiers::SHIFT)),
            [KeyCode::BackTab]
        );
        let save = form_key(&key(egui::Key::S, egui::Modifiers::COMMAND));
        assert_eq!(
            save,
            [KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)]
        );
    }

    #[test]
    fn a_masked_field_shows_one_dot_per_character() {
        let input = TextInput::new("sêcret");
        assert_eq!(shown(&input, true), "••••••");
        assert_eq!(shown(&input, false), "sêcret");
    }
}

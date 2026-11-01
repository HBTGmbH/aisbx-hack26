//! The window's look: its fonts, its type scale and spacing, and its colours per light or dark
//! visuals. Ink marks what acts; colour marks only a state or an agent or kit family. Nothing
//! outside this module should reach for a `Color32` directly.

use aisbx_core::model::{self, Hue};
use aisbx_core::stats::Level;
use eframe::egui::{self, Color32, CornerRadius, Stroke};

/// Text sizes: the body, the gloss under a value, a heading, a section title, and the title of
/// the sandbox page — the one name the main screen is about.
pub const BODY: f32 = 14.0;
pub const SMALL: f32 = 12.0;
pub const HEADING: f32 = 20.0;
pub const SECTION: f32 = 15.0;
pub const TITLE: f32 = 24.0;

/// The fonts and the style of both themes, once, before the first frame.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(fonts());
    ctx.all_styles_mut(style);
}

/// One theme's style: the type scale, the room around controls, and the colours of its visuals.
pub fn style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Small,
            FontId::new(SMALL, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(BODY, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(BODY, FontFamily::Proportional),
        ),
        (TextStyle::Heading, FontId::new(HEADING, strong_family())),
        (
            TextStyle::Monospace,
            FontId::new(13.0, FontFamily::Monospace),
        ),
    ]
    .into();
    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(8.0, 6.0);
    spacing.button_padding = egui::vec2(10.0, 4.0);
    spacing.interact_size.y = 24.0;
    spacing.window_margin = egui::Margin::same(18);
    spacing.menu_margin = egui::Margin::same(8);

    let p = Palette::of(style.visuals.dark_mode);
    let v = &mut style.visuals;
    v.panel_fill = p.ground;
    v.window_fill = p.raised;
    v.extreme_bg_color = p.field;
    v.faint_bg_color = p.stripe;
    // Behind a `code` span (`with_code`): a button's grey, on which every text and hue reads.
    v.code_bg_color = p.button;
    v.window_stroke = Stroke::new(1.0, p.line);
    v.window_corner_radius = CornerRadius::same(10);
    v.menu_corner_radius = CornerRadius::same(8);
    v.selection.bg_fill = p.ink.gamma_multiply(0.22);
    v.selection.stroke = Stroke::new(1.0, p.ink);
    v.hyperlink_color = p.ink;
    v.weak_text_color = Some(p.weak);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.ground;
    w.noninteractive.weak_bg_fill = p.ground;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.weak_bg_fill = p.button;
    w.inactive.bg_fill = p.button;
    w.hovered.weak_bg_fill = p.button_hover;
    w.hovered.bg_fill = p.button_hover;
    w.active.weak_bg_fill = p.button_hover;
    for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        state.corner_radius = CornerRadius::same(6);
    }
}

/// The neutral colours of one theme.
struct Palette {
    /// The window's ground, under the list.
    ground: Color32,
    /// What sits on the ground: the sandbox's page, a dialog.
    raised: Color32,
    /// An input's well.
    field: Color32,
    /// Every other row of a striped grid.
    stripe: Color32,
    line: Color32,
    text: Color32,
    /// Secondary text: 4.5:1 or more on every fill above.
    weak: Color32,
    button: Color32,
    button_hover: Color32,
    ink: Color32,
}

impl Palette {
    fn of(dark: bool) -> Self {
        let rgb = Color32::from_rgb;
        match dark {
            true => Palette {
                ground: rgb(0x14, 0x1b, 0x26),
                raised: rgb(0x1a, 0x23, 0x31),
                field: rgb(0x0f, 0x15, 0x1e),
                stripe: rgb(0x1f, 0x2a, 0x3a),
                line: rgb(0x2e, 0x3c, 0x52),
                text: rgb(0xcf, 0xd8, 0xe6),
                weak: rgb(0x9a, 0xa8, 0xbd),
                button: rgb(0x24, 0x31, 0x47),
                button_hover: rgb(0x2e, 0x3e, 0x59),
                ink: rgb(0xdc, 0xe4, 0xf0),
            },
            false => Palette {
                ground: rgb(0xf0, 0xf3, 0xf8),
                raised: rgb(0xfa, 0xfb, 0xfd),
                field: rgb(0xff, 0xff, 0xff),
                stripe: rgb(0xee, 0xf2, 0xf7),
                line: rgb(0xc8, 0xd2, 0xe0),
                text: rgb(0x24, 0x32, 0x4a),
                weak: rgb(0x4c, 0x5a, 0x72),
                button: rgb(0xdb, 0xe3, 0xee),
                button_hover: rgb(0xcb, 0xd6, 0xe6),
                ink: rgb(0x20, 0x2a, 0x38),
            },
        }
    }
}

/// The sandbox page's and a dialog's fill: a step up from the ground the list sits on.
pub fn raised(dark: bool) -> Color32 {
    Palette::of(dark).raised
}

/// A hairline: a group's outline, the rule between two of its items.
pub fn line(dark: bool) -> Color32 {
    Palette::of(dark).line
}

/// The window's text face, Atkinson Hyperlegible Next: one variable file with every weight from
/// 200 to 800, under the SIL Open Font License (`gui/assets/AtkinsonHyperlegibleNext-OFL.txt`).
const FACE: &[u8] = include_bytes!("../assets/AtkinsonHyperlegibleNext.ttf");

/// The font family of the strong weight ([`strong`]).
const STRONG: &str = "strong";

/// The weight of [`strong`] text, on the face's `wght` axis (regular is 400).
pub const STRONG_WEIGHT: f32 = 600.0;

/// The window's fonts: the [`FACE`] in its regular and its strong weight, then Ubuntu-Light for
/// Greek and Cyrillic, Hack for arrows, dots and triangles, then egui's emoji fonts.
pub fn fonts() -> egui::FontDefinitions {
    use egui::epaint::text::{FontTweak, VariationCoords};
    use egui::{FontData, FontFamily};
    let mut fonts = egui::FontDefinitions::default();
    let strong = FontTweak {
        coords: VariationCoords::new([(b"wght", STRONG_WEIGHT)]),
        ..FontTweak::default()
    };
    fonts.font_data.insert(
        "Atkinson".to_owned(),
        std::sync::Arc::new(FontData::from_static(FACE)),
    );
    fonts.font_data.insert(
        "Atkinson-strong".to_owned(),
        std::sync::Arc::new(FontData::from_static(FACE).tweak(strong)),
    );
    // Before the emoji fonts, whose arrows and dots are a different size and weight.
    let after = |face: &str| -> Vec<String> {
        let mut family = vec![
            face.to_owned(),
            "Ubuntu-Light".to_owned(),
            "Hack".to_owned(),
        ];
        let default = fonts.families.get(&FontFamily::Proportional);
        let rest = default
            .into_iter()
            .flatten()
            .filter(|f| *f != "Ubuntu-Light");
        family.extend(rest.cloned());
        family
    };
    let (regular, strong) = (after("Atkinson"), after("Atkinson-strong"));
    fonts.families.insert(FontFamily::Proportional, regular);
    fonts
        .families
        .insert(FontFamily::Name(STRONG.into()), strong);
    fonts
}

/// The strong weight's family, for a [`egui::FontId`] a painter lays out.
pub fn strong_family() -> egui::FontFamily {
    egui::FontFamily::Name(STRONG.into())
}

/// `text` in the strong weight and colour: a title, a heading, a main button, the open tab.
/// egui's own `RichText::strong` only brightens the colour.
pub fn strong(text: egui::RichText) -> egui::RichText {
    text.strong().family(strong_family())
}

/// A family's colour: brighter on a dark background, deeper on a light one, so both read.
pub fn hue(hue: Hue, dark: bool) -> Color32 {
    let (d, l) = match hue {
        Hue::Red => ((0xf4, 0x76, 0x76), (0xc0, 0x1c, 0x28)),
        Hue::Green => ((0x6c, 0xcb, 0x5f), (0x13, 0x6b, 0x30)),
        Hue::Blue => ((0x6c, 0x9e, 0xf8), (0x1a, 0x56, 0xb8)),
        Hue::Yellow => ((0xe5, 0xc0, 0x4b), (0x80, 0x55, 0x00)),
        Hue::Magenta => ((0xd6, 0x7c, 0xe8), (0x8e, 0x2d, 0xa8)),
        Hue::Cyan => ((0x4f, 0xc8, 0xd8), (0x0a, 0x6c, 0x7c)),
        Hue::LightGreen => ((0xa4, 0xe5, 0x7a), (0x3b, 0x6e, 0x17)),
    };
    let (r, g, b) = if dark { d } else { l };
    Color32::from_rgb(r, g, b)
}

/// A kit badge's colour (`web` red, the host-folder kits one blue, `jdk*` yellow, …).
pub fn kit_color(kit: &str, dark: bool) -> Color32 {
    hue(model::kit_hue(kit), dark)
}

/// The backend line's colour, keyed on the sbx agent.
pub fn agent_color(agent: &str, dark: bool) -> Color32 {
    hue(model::agent_hue(agent), dark)
}

/// The status dot and status line of a running sandbox.
pub fn running(dark: bool) -> Color32 {
    hue(Hue::Green, dark)
}

/// The status dot of a stopped sandbox.
pub fn stopped(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x8a, 0x8f, 0x98)
    } else {
        Color32::from_rgb(0x62, 0x67, 0x71)
    }
}

/// The selected row's band: a tenth of the ink over the ground, light enough that every text the
/// row draws keeps 4.5:1 on it — the strip down its left edge is what marks it.
pub fn band(dark: bool) -> Color32 {
    accent(dark).gamma_multiply(0.1)
}

/// Something out of date or waiting: kit drift, approvals, a missing start folder.
pub fn warn(dark: bool) -> Color32 {
    hue(Hue::Yellow, dark)
}

/// Something that stops the sandbox from working: no token, not responding, an error.
pub fn danger(dark: bool) -> Color32 {
    hue(Hue::Red, dark)
}

/// What acts: the main buttons, the selected row's strip and tint, the open tab and core's
/// cursor. The ink of the text, not a hue: colour on screen means a state or a family.
pub fn accent(dark: bool) -> Color32 {
    Palette::of(dark).ink
}

/// Text on an [`accent`] fill: the ground's dark on the light ink of dark mode, white on the dark
/// ink of light mode.
pub fn on_accent(dark: bool) -> Color32 {
    match dark {
        true => Color32::from_rgb(0x14, 0x1b, 0x26),
        false => Color32::WHITE,
    }
}

/// Text on a [`danger`] fill: the confirming button of what cannot be undone.
pub fn on_danger(dark: bool) -> Color32 {
    match dark {
        true => Color32::from_rgb(0x1c, 0x0c, 0x0d),
        false => Color32::WHITE,
    }
}

/// A tint of `color` behind text in that colour: a badge, a banner, the attention callout.
pub fn tint(color: Color32, dark: bool) -> Color32 {
    color.gamma_multiply(if dark { 0.16 } else { 0.12 })
}

/// A badge's fill: `color`'s [`tint`] laid over the page's raised colour, opaque, so a badge
/// reads the same on a plain row, on the selected row's band and on the page.
pub fn badge_fill(color: Color32, dark: bool) -> Color32 {
    opaque(tint(color, dark), dark)
}

/// A running sandbox's chamber on the map: half a badge's tint of its agent's `color` over the
/// raised colour, opaque, light enough for every kit's colour inside it.
pub fn wash(color: Color32, dark: bool) -> Color32 {
    opaque(tint(color, dark).gamma_multiply(0.5), dark)
}

/// `tint` laid over the raised colour.
fn opaque(tint: Color32, dark: bool) -> Color32 {
    let base = raised(dark);
    let rest = 1.0 - tint.a() as f32 / 255.0;
    let mix = |t: u8, b: u8| (t as f32 + b as f32 * rest).round() as u8;
    Color32::from_rgb(
        mix(tint.r(), base.r()),
        mix(tint.g(), base.g()),
        mix(tint.b(), base.b()),
    )
}

/// Glyphs that text written for the terminal carries and no font of the window has, and what is
/// drawn in their place: in a kit's description and in what sbx prints into a job's log.
const STAND_INS: [(char, char); 4] = [('⬢', '●'), ('✓', '✔'), ('✗', '×'), ('✕', '×')];

/// `text` with every glyph the window has no font for ([`STAND_INS`]) replaced by its stand-in,
/// so none is drawn as an empty box.
pub fn drawable(text: &str) -> std::borrow::Cow<'_, str> {
    let stand_in = |c: char| {
        STAND_INS
            .iter()
            .find(|(glyph, _)| *glyph == c)
            .map(|(_, s)| *s)
    };
    match text.chars().any(|c| stand_in(c).is_some()) {
        true => text.chars().map(|c| stand_in(c).unwrap_or(c)).collect(),
        false => std::borrow::Cow::Borrowed(text),
    }
}

/// `text` split at its pairs of backticks into `(piece, code)`, in order: core marks a command, a
/// path or a name as `code`, as its TUI shows it. A backtick without a partner, and a pair with
/// nothing between, are text.
pub fn code_pieces(text: &str) -> Vec<(&str, bool)> {
    let mut pieces = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let Some(len) = rest[open + 1..].find('`').filter(|len| *len > 0) else {
            break;
        };
        if open > 0 {
            pieces.push((&rest[..open], false));
        }
        pieces.push((&rest[open + 1..open + 1 + len], true));
        rest = &rest[open + 2 + len..];
    }
    if !rest.is_empty() || pieces.is_empty() {
        pieces.push((rest, false));
    }
    pieces
}

/// `text` for a label, as `style` makes it, each `code` piece ([`code_pieces`]) in the monospace
/// face on the code fill, without its backticks: a command reads as one. Text without a pair of
/// backticks is `style`'s `RichText`, unchanged.
pub fn with_code(
    ui: &egui::Ui,
    text: &str,
    style: impl Fn(egui::RichText) -> egui::RichText,
) -> egui::WidgetText {
    let pieces = code_pieces(text);
    if pieces.iter().all(|(_, code)| !code) {
        return style(egui::RichText::new(text)).into();
    }
    let mut job = egui::text::LayoutJob::default();
    for (piece, code) in pieces {
        let rich = style(egui::RichText::new(piece));
        // The monospace face even where `style` named a family ([`strong`]).
        let rich = match code {
            true => rich.code().family(egui::FontFamily::Monospace),
            false => rich,
        };
        let font = egui::FontSelection::Default;
        rich.append_to(&mut job, ui.style(), font, egui::Align::Center);
    }
    job.into()
}

/// [`with_code`] for text a painter lays out itself, in `font` and `color`: a code piece a point
/// smaller in the monospace face, which is the wider of the two.
pub fn code_job(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for (piece, code) in code_pieces(text) {
        let format = match code {
            true => egui::TextFormat {
                font_id: egui::FontId::monospace(font.size - 1.0),
                background: ui.visuals().code_bg_color,
                ..egui::TextFormat::simple(font.clone(), color)
            },
            false => egui::TextFormat::simple(font.clone(), color),
        };
        job.append(piece, 0.0, format);
    }
    job
}

/// A meter's or a chart's colour, by how full it is (core's `stats::level`): the running green,
/// then the warning yellow, then the danger red — each readable on both backgrounds.
pub fn level(level: Level, dark: bool) -> Color32 {
    match level {
        Level::Normal => running(dark),
        Level::High => warn(dark),
        Level::Critical => danger(dark),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui;

    /// WCAG's contrast ratio of two opaque colours.
    fn contrast(a: Color32, b: Color32) -> f32 {
        fn luminance(c: Color32) -> f32 {
            let channel = |v: u8| {
                let v = v as f32 / 255.0;
                match v <= 0.039_28 {
                    true => v / 12.92,
                    false => ((v + 0.055) / 1.055).powf(2.4),
                }
            };
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// `fill`, a premultiplied colour, painted over the opaque `ground`.
    fn over(fill: Color32, ground: Color32) -> Color32 {
        let rest = 1.0 - fill.a() as f32 / 255.0;
        let mix = |f: u8, g: u8| (f as f32 + g as f32 * rest).round() as u8;
        Color32::from_rgb(
            mix(fill.r(), ground.r()),
            mix(fill.g(), ground.g()),
            mix(fill.b(), ground.b()),
        )
    }

    const HUES: [Hue; 7] = [
        Hue::Red,
        Hue::Green,
        Hue::Blue,
        Hue::Yellow,
        Hue::Magenta,
        Hue::Cyan,
        Hue::LightGreen,
    ];

    fn installed() -> egui::Context {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx
    }

    /// WCAG AA: 4.5:1 for text, 3:1 for the stopped dot, which is a graphic.
    #[test]
    fn every_text_reads_on_every_fill() {
        let ctx = installed();
        for dark in [true, false] {
            let theme = if dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            };
            let style = ctx.style_of(theme);
            let v = &style.visuals;
            let ground = v.panel_fill;
            let fills = [
                ("ground", ground),
                ("raised", raised(dark)),
                ("stripe", v.faint_bg_color),
                ("button", v.widgets.inactive.weak_bg_fill),
                ("band", over(band(dark), ground)),
                ("code", v.code_bg_color),
            ];
            let mut texts = vec![
                ("text", v.text_color(), 4.5),
                ("weak", v.weak_text_color(), 4.5),
                ("strong", v.strong_text_color(), 4.5),
                ("stopped", stopped(dark), 3.0),
            ];
            texts.extend(HUES.map(|h| ("hue", hue(h, dark), 4.5)));
            for (fill_name, fill) in fills {
                for (text_name, text, least) in &texts {
                    let ratio = contrast(*text, fill);
                    assert!(
                        ratio >= *least,
                        "dark {dark}: {text_name} {text:?} on {fill_name}: {ratio:.2}"
                    );
                }
            }
            // A colour on its own badge (opaque) and on its own callout (a tint over the page).
            let weak = v.weak_text_color();
            let tags = [weak, accent(dark)];
            for colour in HUES.map(|h| hue(h, dark)).into_iter().chain(tags) {
                let fill = badge_fill(colour, dark);
                assert_eq!(fill.a(), 255, "dark {dark}: a badge is opaque");
                let ratio = contrast(colour, fill);
                assert!(
                    ratio >= 4.5,
                    "dark {dark}: {colour:?} on its badge: {ratio:.2}"
                );
                let callout = over(tint(colour, dark), raised(dark));
                let ratio = contrast(colour, callout);
                assert!(
                    ratio >= 4.5,
                    "dark {dark}: {colour:?} on its callout: {ratio:.2}"
                );
            }
            // A running sandbox's chamber on the map: its agent's badge fill, under the agent's
            // name and every kit's colour.
            for agent in HUES {
                let chamber = wash(hue(agent, dark), dark);
                let names = [("strong", v.strong_text_color())];
                let kits = HUES.map(|h| ("kit hue", hue(h, dark)));
                for (name, text) in names.into_iter().chain(kits) {
                    let ratio = contrast(text, chamber);
                    assert!(
                        ratio >= 4.5,
                        "dark {dark}: {name} {text:?} in the {agent:?} chamber: {ratio:.2}"
                    );
                }
            }
            // Text inside the warning and danger callouts: not the weak grey, under 4.5:1 there.
            for colour in [warn(dark), danger(dark)] {
                let callout = over(tint(colour, dark), raised(dark));
                let inside = [("text", v.text_color()), ("strong", v.strong_text_color())];
                for (name, text) in inside {
                    let ratio = contrast(text, callout);
                    assert!(
                        ratio >= 4.5,
                        "dark {dark}: {name} on the {colour:?} callout: {ratio:.2}"
                    );
                }
            }
            // egui writes a progress bar's text in the selection's stroke colour.
            let track = v.faint_bg_color;
            let bar = over(v.selection.bg_fill, track);
            for (name, text, fill) in [
                ("on accent", on_accent(dark), accent(dark)),
                ("on danger", on_danger(dark), danger(dark)),
                ("bar text on its track", v.selection.stroke.color, track),
                ("bar text on its fill", v.selection.stroke.color, bar),
            ] {
                let ratio = contrast(text, fill);
                assert!(ratio >= 4.5, "dark {dark}: {name}: {ratio:.2}");
            }
        }
    }

    #[test]
    fn the_strong_weight_is_the_faces_own() {
        use egui::FontFamily;
        let fonts = fonts();
        let family = |f: &FontFamily| fonts.families.get(f).cloned().unwrap_or_default();
        let regular = family(&FontFamily::Proportional);
        let heavy = family(&strong_family());
        assert_eq!(regular.first().map(String::as_str), Some("Atkinson"));
        assert_eq!(heavy.first().map(String::as_str), Some("Atkinson-strong"));
        assert_eq!(regular[1..], heavy[1..], "the same fallbacks");
        assert!(regular.contains(&"Hack".to_string()));
        let data = |name: &str| fonts.font_data.get(name).unwrap().clone();
        assert_eq!(data("Atkinson").bytes(), FACE);
        assert_eq!(
            data("Atkinson-strong").bytes(),
            FACE,
            "one file, two weights"
        );
        let coords = |name: &str| data(name).tweak.coords.as_ref().to_vec();
        assert!(coords("Atkinson").is_empty(), "the face's own regular");
        let wght = egui::epaint::text::VariationCoords::new([(b"wght", STRONG_WEIGHT)]);
        assert_eq!(coords("Atkinson-strong"), wght.as_ref());
        let axes = data("Atkinson").variation_axes();
        let axis = axes
            .iter()
            .find(|a| a.tag == egui::epaint::text::Tag::new(b"wght"))
            .expect("a weight axis");
        assert!(axis.range.contains(STRONG_WEIGHT) && axis.default < STRONG_WEIGHT);
        let text = strong(egui::RichText::new("x"));
        let ctx = installed();
        let job = egui::WidgetText::from(text).into_layout_job(
            &ctx.global_style(),
            egui::FontSelection::Default,
            egui::Align::Min,
        );
        assert_eq!(job.sections[0].format.font_id.family, strong_family());
    }

    /// A kit's text is data, which the window's own symbol list does not cover.
    #[test]
    fn every_kits_text_has_a_font() {
        let ctx = installed();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("x");
        });
        output.textures_delta.clear();
        let font = egui::FontId::proportional(BODY);
        let catalog = aisbx_core::kits::Catalog::load().unwrap();
        let kits = (catalog.backends.iter().map(|b| &b.kit))
            .chain(catalog.addons.iter().map(|a| &a.kit))
            .chain(catalog.headless.iter().map(|a| &a.kit));
        for kit in kits {
            for text in [&kit.display_name, &kit.description] {
                let missing: String = (drawable(text).chars())
                    .filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&font, *c)))
                    .collect();
                assert_eq!(missing, "", "{}: drawn as empty boxes", kit.name);
            }
        }
        assert_eq!(drawable("plain"), "plain");
    }

    #[test]
    fn backticks_split_into_code_pieces() {
        assert_eq!(
            code_pieces("run `sbx setup ssh` first"),
            [("run ", false), ("sbx setup ssh", true), (" first", false)]
        );
        assert_eq!(
            code_pieces("`false` failed"),
            [("false", true), (" failed", false)]
        );
        assert_eq!(
            code_pieces("a `b` `c`"),
            [("a ", false), ("b", true), (" ", false), ("c", true)]
        );
        assert_eq!(code_pieces("it's `odd"), [("it's `odd", false)]);
        assert_eq!(
            code_pieces("an `` empty pair"),
            [("an `` empty pair", false)]
        );
        assert_eq!(code_pieces(""), [("", false)]);
    }

    #[test]
    fn a_code_piece_is_monospace_on_the_code_fill() {
        let ctx = installed();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::proportional(BODY);
            let job = code_job(ui, "run `sbx setup ssh` first", font.clone(), Color32::RED);
            assert_eq!(job.text, "run sbx setup ssh first");
            let code: Vec<_> = (job.sections.iter())
                .filter(|s| s.format.font_id.family == egui::FontFamily::Monospace)
                .map(|s| {
                    (
                        &job.text[s.byte_range.start.0..s.byte_range.end.0],
                        s.format.background,
                    )
                })
                .collect();
            assert_eq!(code, [("sbx setup ssh", ui.visuals().code_bg_color)]);
            assert!(job.sections.iter().all(|s| s.format.color == Color32::RED));
            let widget = with_code(ui, "`false` failed", |t| t.color(Color32::RED));
            assert_eq!(widget.text(), "false failed");
            let heading = with_code(ui, "the kits' `credentials:`", strong);
            let egui::WidgetText::LayoutJob(job) = heading else {
                panic!("a heading with code is a layout job");
            };
            let family = |piece: &str| {
                let at = job.text.find(piece).unwrap();
                let s = job
                    .sections
                    .iter()
                    .find(|s| s.byte_range.start.0 == at)
                    .unwrap();
                s.format.font_id.family.clone()
            };
            assert_eq!(family("credentials:"), egui::FontFamily::Monospace);
            assert_eq!(family("the kits' "), strong_family());
            let plain = with_code(ui, "no code here", |t| t);
            assert!(
                matches!(plain, egui::WidgetText::RichText(_)),
                "left as it was"
            );
        });
        output.textures_delta.clear();
    }

    #[test]
    fn text_and_targets_have_a_readable_size() {
        let ctx = installed();
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = ctx.style_of(theme);
            let smallest = style
                .text_styles
                .values()
                .map(|f| f.size)
                .fold(f32::MAX, f32::min);
            assert!(smallest >= 12.0, "{theme:?}: {smallest}");
            assert!(style.spacing.interact_size.y >= 24.0, "{theme:?}");
        }
    }

    #[test]
    fn the_window_acts_in_ink_not_in_the_terminals_cyan() {
        for dark in [true, false] {
            let a = accent(dark);
            let (hi, lo) = [a.r(), a.g(), a.b()]
                .iter()
                .fold((0u8, 255u8), |(hi, lo), &c| (hi.max(c), lo.min(c)));
            assert!(hi - lo <= 24, "dark {dark}: {a:?} is a hue");
            assert!(HUES.iter().all(|h| hue(*h, dark) != a), "dark {dark}");
        }
    }

    #[test]
    fn every_symbol_the_window_draws_has_a_font() {
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts());
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.label("x");
        });
        output.textures_delta.clear();
        let font = egui::FontId::proportional(14.0);
        let missing: String = "●○◉⚑⟳⚠▾▸↵→←↑↓⬆·›‹—–…×−≥±☑☐⏵⏸🗁🗀"
            .chars()
            .filter(|c| !ctx.fonts_mut(|f| f.has_glyph(&font, *c)))
            .collect();
        assert_eq!(missing, "", "drawn as empty boxes");
        // A glyph without a font draws through its stand-in, also in the log's monospace face.
        let log = egui::FontId::monospace(12.0);
        for glyph in ['✓', '✗', '✕', '⬢'] {
            assert!(
                !ctx.fonts_mut(|f| f.has_glyph(&log, glyph)),
                "{glyph} needs no stand-in"
            );
            let stand_in = drawable(&glyph.to_string()).into_owned();
            for font in [&font, &log] {
                let has = |c| ctx.fonts_mut(|f| f.has_glyph(font, c));
                assert!(stand_in.chars().all(has), "{glyph} as {stand_in}");
            }
        }
    }

    #[test]
    fn the_host_folder_kits_share_one_colour() {
        for dark in [true, false] {
            for name in ["host-maven", "host-npm", "host-pip", "host-skills"] {
                assert_eq!(
                    kit_color(name, dark),
                    kit_color("host-gradle", dark),
                    "{name}"
                );
            }
            assert_ne!(kit_color("web", dark), kit_color("host-gradle", dark));
        }
    }
}

//! Colour vocabulary for the whole UI, from the 16 named terminal colours only, and the fallback
//! for a terminal without colour or Unicode.

use crate::model::{self, Hue};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};

pub const ACCENT: Color = Color::Cyan;
pub const MUTED: Color = Color::DarkGray;
pub const RUNNING: Color = Color::Green;
pub const STOPPED: Color = Color::DarkGray;
pub const WARN: Color = Color::Yellow;
pub const DANGER: Color = Color::Red;

/// Per-kit badge colour; [`model::kit_hue`] picks the hue.
pub fn kit_color(kit: &str) -> Color {
    named(model::kit_hue(kit))
}

/// Backend badge colour, keyed on the sbx agent the kit mixes onto ([`model::agent_hue`]).
pub fn agent_color(agent: &str) -> Color {
    named(model::agent_hue(agent))
}

fn named(hue: Hue) -> Color {
    match hue {
        Hue::Red => Color::Red,
        Hue::Green => Color::Green,
        Hue::Blue => Color::Blue,
        Hue::Yellow => Color::Yellow,
        Hue::Magenta => Color::Magenta,
        Hue::Cyan => Color::Cyan,
        Hue::LightGreen => Color::LightGreen,
    }
}

pub fn title() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn muted() -> Style {
    Style::default().fg(MUTED)
}

pub fn selected() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// The text colour on the [`selected`] band.
pub const ON_SELECTION: Color = Color::White;

/// A span on the cursor's row, inside the [`selected`] band: [`ON_SELECTION`], whatever its
/// own colour. Bold and the other modifiers stay.
pub fn on_selection(style: Style) -> Style {
    style.fg(ON_SELECTION)
}

/// A keyboard shortcut in the footer / help text.
pub fn key() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

pub fn border() -> Style {
    Style::default().fg(MUTED)
}

pub fn border_focused() -> Style {
    Style::default().fg(ACCENT)
}

pub fn danger() -> Style {
    Style::default().fg(DANGER).add_modifier(Modifier::BOLD)
}

pub fn warn() -> Style {
    Style::default().fg(WARN)
}

/// What the terminal can show: colour, and characters beyond ASCII.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    pub color: bool,
    pub unicode: bool,
}

impl Caps {
    pub const FULL: Self = Self {
        color: true,
        unicode: true,
    };

    /// A non-empty `NO_COLOR` (no-color.org) turns colour off. The first of `LC_ALL`, `LC_CTYPE`
    /// and `LANG` that is set must name UTF-8, else the characters are ASCII.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |name: &str| var(name).filter(|v| !v.is_empty());
        let unicode = match ["LC_ALL", "LC_CTYPE", "LANG"].into_iter().find_map(set) {
            Some(locale) => {
                let locale = locale.to_ascii_lowercase();
                locale.contains("utf-8") || locale.contains("utf8")
            }
            None => true,
        };
        Self {
            color: set("NO_COLOR").is_none(),
            unicode,
        }
    }
}

/// This terminal's [`Caps`], read once. The tests always draw with everything on, whatever the
/// environment they run in.
pub fn caps() -> Caps {
    if cfg!(test) {
        return Caps::FULL;
    }
    static CAPS: std::sync::OnceLock<Caps> = std::sync::OnceLock::new();
    *CAPS.get_or_init(|| Caps::from_env(|name| std::env::var(name).ok()))
}

/// Take a drawn frame down to what the terminal can show, cell by cell, so no layout moves.
/// Without colour, a coloured background becomes reverse video and a reversed cell turns back.
pub fn degrade(buffer: &mut Buffer, caps: Caps) {
    if caps == Caps::FULL {
        return;
    }
    for cell in buffer.content.iter_mut() {
        if !caps.color {
            if cell.bg != Color::Reset {
                cell.modifier.toggle(Modifier::REVERSED);
            }
            cell.fg = Color::Reset;
            cell.bg = Color::Reset;
        }
        if !caps.unicode {
            if let Some(plain) = ascii(cell.symbol()) {
                cell.set_symbol(plain);
            }
        }
    }
}

/// The ASCII stand-in for one cell's symbol, one column wide like the symbols it replaces; `None`
/// for ASCII itself and for a wide character, whose second column a stand-in would not fill.
pub fn ascii(symbol: &str) -> Option<&'static str> {
    let mut chars = symbol.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return None;
    };
    if c.is_ascii() {
        return None;
    }
    Some(match c {
        '─' | '━' | '═' | '—' | '–' | '−' | '╌' => "-",
        '│' | '┃' | '║' | '▏' | '▎' | '▍' | '▌' | '▋' | '▊' | '▉' | '▐' => {
            "|"
        }
        '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' | '├' | '┤' | '┬' | '┴' | '┼' => {
            "+"
        }
        '◉' | '●' | '•' | '✓' | '✔' | '⬢' => "*",
        '○' | '◯' => "o",
        '✗' | '✕' | '×' => "x",
        '⚠' | '⚑' => "!",
        '›' | '»' | '▸' | '▶' | '→' | '⏵' => ">",
        '‹' | '«' | '◂' | '←' | '↵' => "<",
        '▾' | '▼' | '↓' => "v",
        '↑' | '▲' => "^",
        '↻' | '⟳' => "~",
        '·' => ".",
        // Not `.`, which would end the sentence a cut text was cut out of.
        '…' => "~",
        '“' | '”' | '„' => "\"",
        '‘' | '’' => "'",
        '≥' => ">",
        '≤' => "<",
        '±' => "+",
        '█' | '▓' | '▒' | '░' => "#",
        // A running job's spinner, frame by frame: a turning bar.
        '⣾' | '⡿' => "|",
        '⣽' | '⣟' => "/",
        '⣻' | '⣯' => "-",
        '⢿' | '⣷' => "\\",
        // A sparkline's cells, lowest to highest, in characters that rise as they do.
        '▁' | '▂' => "_",
        '▃' => ".",
        '▄' | '▅' => "-",
        '▆' => "=",
        '▇' => "#",
        c if unicode_width(c) != 1 => return None,
        _ => "?",
    })
}

/// How many columns `c` takes on screen, as ratatui counts them.
pub fn unicode_width(c: char) -> usize {
    ratatui::text::Span::raw(c.to_string()).width()
}

/// A key as the footer and the help sheet name it: the arrows, ↵, ⇧ and … as words where the
/// terminal has no Unicode, and not the one-character stand-ins [`degrade`] would draw.
pub fn key_name(key: &str, caps: Caps) -> String {
    match caps.unicode {
        true => key.to_string(),
        false => key
            .replace("↑↓←→", "arrows")
            .replace("↑↓", "up/down")
            .replace("←→", "left/right")
            .replace('↵', "enter")
            .replace('⇧', "shift-")
            .replace('…', "..."),
    }
}

#[cfg(test)]
mod tests {
    use super::{kit_color, muted, on_selection, warn};
    use ratatui::style::Color;

    #[test]
    fn every_colour_turns_bright_white_on_the_selection_band() {
        use ratatui::style::{Modifier, Style};
        for style in [
            muted(),
            warn(),
            Style::default(),
            Style::default().fg(kit_color("host-gradle")),
        ] {
            assert_eq!(on_selection(style).fg, Some(Color::White), "{style:?}");
        }
        let bold = warn().add_modifier(Modifier::BOLD);
        assert!(on_selection(bold).add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn the_terminals_caps_come_from_no_color_and_the_locale() {
        use super::Caps;
        let env = |pairs: &'static [(&str, &str)]| {
            Caps::from_env(move |name| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(env(&[]), Caps::FULL);
        assert!(!env(&[("NO_COLOR", "1")]).color);
        assert!(env(&[("NO_COLOR", "")]).color, "empty is unset");
        assert!(env(&[("LANG", "en_US.UTF-8")]).unicode);
        assert!(env(&[("LANG", "de_DE.utf8")]).unicode);
        assert!(!env(&[("LANG", "C")]).unicode);
        assert!(!env(&[("LC_ALL", "C"), ("LANG", "en_US.UTF-8")]).unicode);
        assert!(!env(&[("LC_CTYPE", "POSIX"), ("LANG", "en_US.UTF-8")]).unicode);
        assert!(env(&[("LC_ALL", ""), ("LANG", "en_US.UTF-8")]).unicode);
    }

    #[test]
    fn a_degraded_frame_keeps_its_layout_and_its_cursor() {
        use super::{ascii, degrade, Caps};
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::style::{Modifier, Style};
        let mut buffer = Buffer::empty(Rect::new(0, 0, 6, 1));
        buffer.set_string(0, 0, "›◉ ab", super::selected());
        buffer[(3, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
        degrade(
            &mut buffer,
            Caps {
                color: false,
                unicode: false,
            },
        );
        let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
        assert_eq!(text, ">* ab ");
        assert!(buffer
            .content
            .iter()
            .all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        let reversed = |x: u16| buffer[(x, 0)].modifier.contains(Modifier::REVERSED);
        assert!(reversed(0) && reversed(2), "the band");
        assert!(!reversed(3), "the caret in it");
        assert!(!reversed(5), "no band, no reverse");
        assert_eq!(ascii("a"), None);
        assert_eq!(ascii("─"), Some("-"));
        assert_eq!(ascii("漢"), None, "a wide character stays");
        assert_eq!(ascii("ä"), Some("?"));
    }

    #[test]
    fn the_host_folder_kits_share_one_colour() {
        for name in ["host-maven", "host-npm", "host-pip", "host-skills"] {
            assert_eq!(kit_color(name), kit_color("host-gradle"));
        }
    }
}

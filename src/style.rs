//! Colors, modifiers and the `Style` that combines them.

use std::ops::{BitOr, BitOrAssign};

/// A terminal color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's default foreground or background.
    Default,
    /// ANSI black.
    Black,
    /// ANSI red.
    Red,
    /// ANSI green.
    Green,
    /// ANSI yellow.
    Yellow,
    /// ANSI blue.
    Blue,
    /// ANSI magenta.
    Magenta,
    /// ANSI cyan.
    Cyan,
    /// ANSI white (light gray on most themes).
    White,
    /// Bright black (dark gray).
    BrightBlack,
    /// Bright red.
    BrightRed,
    /// Bright green.
    BrightGreen,
    /// Bright yellow.
    BrightYellow,
    /// Bright blue.
    BrightBlue,
    /// Bright magenta.
    BrightMagenta,
    /// Bright cyan.
    BrightCyan,
    /// Bright white.
    BrightWhite,
    /// An entry of the 256-color palette.
    Indexed(u8),
    /// A 24-bit color.
    Rgb(u8, u8, u8),
}

/// A set of text attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifier(u8);

impl Modifier {
    /// No attributes.
    pub const NONE: Modifier = Modifier(0);
    /// Bold.
    pub const BOLD: Modifier = Modifier(1);
    /// Dim (faint).
    pub const DIM: Modifier = Modifier(1 << 1);
    /// Italic.
    pub const ITALIC: Modifier = Modifier(1 << 2);
    /// Underline.
    pub const UNDERLINE: Modifier = Modifier(1 << 3);
    /// Swapped foreground and background.
    pub const REVERSE: Modifier = Modifier(1 << 4);
    /// Strikethrough.
    pub const STRIKETHROUGH: Modifier = Modifier(1 << 5);

    /// True when every attribute in `other` is set.
    pub const fn contains(self, other: Modifier) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no attribute is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The attributes in `self` that are not in `other`.
    pub const fn difference(self, other: Modifier) -> Modifier {
        Modifier(self.0 & !other.0)
    }
}

impl BitOr for Modifier {
    type Output = Modifier;
    fn bitor(self, rhs: Modifier) -> Modifier {
        Modifier(self.0 | rhs.0)
    }
}

impl BitOrAssign for Modifier {
    fn bitor_assign(&mut self, rhs: Modifier) {
        self.0 |= rhs.0;
    }
}

/// Foreground, background and text attributes.
///
/// `None` for a color means "leave whatever is underneath", which is what
/// makes [`Style::patch`] and `Buffer::set_style` composable. The type is
/// `Copy` and small enough to pass around freely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    /// Foreground color, if set.
    pub fg: Option<Color>,
    /// Background color, if set.
    pub bg: Option<Color>,
    /// Attributes that are turned on.
    pub modifiers: Modifier,
}

impl Style {
    /// An empty style that changes nothing.
    pub const fn new() -> Self {
        Style {
            fg: None,
            bg: None,
            modifiers: Modifier::NONE,
        }
    }

    /// Sets the foreground color.
    pub const fn fg(mut self, color: Color) -> Self {
        self.fg = Some(color);
        self
    }

    /// Sets the background color.
    pub const fn bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    /// Turns on the attributes in `modifier`.
    pub const fn add_modifier(mut self, modifier: Modifier) -> Self {
        self.modifiers = Modifier(self.modifiers.0 | modifier.0);
        self
    }

    /// Bold.
    pub const fn bold(self) -> Self {
        self.add_modifier(Modifier::BOLD)
    }

    /// Dim.
    pub const fn dim(self) -> Self {
        self.add_modifier(Modifier::DIM)
    }

    /// Italic.
    pub const fn italic(self) -> Self {
        self.add_modifier(Modifier::ITALIC)
    }

    /// Underline.
    pub const fn underline(self) -> Self {
        self.add_modifier(Modifier::UNDERLINE)
    }

    /// Reverse video.
    pub const fn reverse(self) -> Self {
        self.add_modifier(Modifier::REVERSE)
    }

    /// Strikethrough.
    pub const fn strikethrough(self) -> Self {
        self.add_modifier(Modifier::STRIKETHROUGH)
    }

    /// Layers `other` on top of `self`: its colors win where set, and its
    /// attributes are added.
    pub const fn patch(self, other: Style) -> Style {
        Style {
            fg: if other.fg.is_some() {
                other.fg
            } else {
                self.fg
            },
            bg: if other.bg.is_some() {
                other.bg
            } else {
                self.bg
            },
            modifiers: Modifier(self.modifiers.0 | other.modifiers.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_stays_small() {
        assert!(
            std::mem::size_of::<Style>() <= 10,
            "{}",
            std::mem::size_of::<Style>()
        );
        assert!(std::mem::size_of::<Color>() <= 4);
    }

    #[test]
    fn builder_chain() {
        let s = Style::new().fg(Color::Cyan).bold().underline();
        assert_eq!(s.fg, Some(Color::Cyan));
        assert!(s.modifiers.contains(Modifier::BOLD | Modifier::UNDERLINE));
        assert!(!s.modifiers.contains(Modifier::ITALIC));
    }

    #[test]
    fn patch_prefers_set_colors_and_unions_modifiers() {
        let base = Style::new().fg(Color::Red).bg(Color::Black).bold();
        let top = Style::new().fg(Color::Green).italic();
        let out = base.patch(top);
        assert_eq!(out.fg, Some(Color::Green));
        assert_eq!(out.bg, Some(Color::Black));
        assert!(out.modifiers.contains(Modifier::BOLD | Modifier::ITALIC));
    }

    #[test]
    fn difference_removes_bits() {
        let m = Modifier::BOLD | Modifier::DIM;
        assert_eq!(m.difference(Modifier::BOLD), Modifier::DIM);
        assert!(Modifier::NONE.is_empty());
    }
}

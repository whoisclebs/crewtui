//! Display-width aware helpers for strings.
//!
//! Terminals lay text out in columns, and a column is not a byte, a `char`
//! or even a grapheme cluster. These helpers work on grapheme clusters and
//! count columns, so wide (CJK, emoji) and zero-width (combining marks)
//! text lines up the way it does on screen.
//!
//! Control characters, including tabs, have zero width and are not drawn.

use std::borrow::Cow;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::Style;

/// Columns a single grapheme cluster occupies: 0, 1 or 2.
pub(crate) fn grapheme_width(g: &str) -> usize {
    if g.chars().next().is_some_and(char::is_control) {
        return 0;
    }
    UnicodeWidthStr::width(g).min(2)
}

/// Number of terminal columns `s` occupies.
pub fn width(s: &str) -> usize {
    s.graphemes(true).map(grapheme_width).sum()
}

/// The longest prefix of `s` that fits in `cols` columns, cut on a grapheme
/// boundary. A wide glyph that would straddle the limit is left out.
pub fn truncate(s: &str, cols: usize) -> &str {
    let mut used = 0;
    for (i, g) in s.grapheme_indices(true) {
        let w = grapheme_width(g);
        if used + w > cols {
            return &s[..i];
        }
        used += w;
    }
    s
}

/// Splits `s` into lines of at most `cols` columns, breaking on grapheme
/// boundaries. Existing `\n` (and `\r\n`) line breaks are kept, so an empty
/// input line yields an empty output line.
///
/// A glyph wider than `cols` gets a line of its own rather than being
/// dropped, so this always makes progress. `cols == 0` yields no lines.
/// Word-aware wrapping is a `Paragraph` concern.
pub fn wrap(s: &str, cols: usize) -> Vec<&str> {
    let mut lines = Vec::new();
    if cols == 0 {
        return lines;
    }
    for raw in s.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let mut start = 0;
        let mut used = 0;
        for (i, g) in raw.grapheme_indices(true) {
            let w = grapheme_width(g);
            if used + w > cols && i > start {
                lines.push(&raw[start..i]);
                start = i;
                used = 0;
            }
            used += w;
        }
        lines.push(&raw[start..]);
    }
    lines
}

/// Where text sits between the left and right edge of the space it has.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HorizontalAlign {
    /// Against the left edge.
    #[default]
    Left,
    /// In the middle. An odd extra cell goes on the right.
    Center,
    /// Against the right edge.
    Right,
}

/// A piece of text with one style.
///
/// The content is a [`Cow`], so text that already lives in the app's state
/// can be drawn without copying it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Span<'a> {
    /// The text.
    pub content: Cow<'a, str>,
    /// The style applied to it.
    pub style: Style,
}

impl<'a> Span<'a> {
    /// Text with no style of its own.
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Span {
            content: content.into(),
            style: Style::new(),
        }
    }

    /// Text with `style`.
    pub fn styled(content: impl Into<Cow<'a, str>>, style: Style) -> Self {
        Span {
            content: content.into(),
            style,
        }
    }

    /// The same text with `style` layered over its current one.
    pub fn style(mut self, style: Style) -> Self {
        self.style = self.style.patch(style);
        self
    }

    /// Columns the text takes.
    pub fn width(&self) -> usize {
        width(&self.content)
    }

    /// The same span, owning its text.
    pub fn into_owned(self) -> Span<'static> {
        Span {
            content: Cow::Owned(self.content.into_owned()),
            style: self.style,
        }
    }
}

impl<'a> From<&'a str> for Span<'a> {
    fn from(s: &'a str) -> Self {
        Span::raw(s)
    }
}

impl From<String> for Span<'_> {
    fn from(s: String) -> Self {
        Span::raw(s)
    }
}

/// A row of spans. It never wraps by itself; a [`Paragraph`] decides that.
///
/// [`Paragraph`]: crate::widgets::Paragraph
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line<'a> {
    /// The pieces, left to right.
    pub spans: Vec<Span<'a>>,
    /// A style applied under every span.
    pub style: Style,
    /// Overrides the paragraph's alignment for this line.
    pub alignment: Option<HorizontalAlign>,
}

impl<'a> Line<'a> {
    /// A line of unstyled text.
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        Line::from(Span::raw(content))
    }

    /// The same line with `style` layered under its spans.
    pub fn style(mut self, style: Style) -> Self {
        self.style = self.style.patch(style);
        self
    }

    /// Sets the alignment of this line.
    pub fn align(mut self, alignment: HorizontalAlign) -> Self {
        self.alignment = Some(alignment);
        self
    }

    /// Columns the whole line takes.
    pub fn width(&self) -> usize {
        self.spans.iter().map(Span::width).sum()
    }

    /// The same line, owning its text.
    pub fn into_owned(self) -> Line<'static> {
        Line {
            spans: self.spans.into_iter().map(Span::into_owned).collect(),
            style: self.style,
            alignment: self.alignment,
        }
    }
}

impl<'a> From<Span<'a>> for Line<'a> {
    fn from(span: Span<'a>) -> Self {
        Line {
            spans: vec![span],
            ..Line::default()
        }
    }
}

impl<'a> From<Vec<Span<'a>>> for Line<'a> {
    fn from(spans: Vec<Span<'a>>) -> Self {
        Line {
            spans,
            ..Line::default()
        }
    }
}

impl<'a> From<&'a str> for Line<'a> {
    fn from(s: &'a str) -> Self {
        Line::raw(s)
    }
}

impl From<String> for Line<'_> {
    fn from(s: String) -> Self {
        Line::raw(s)
    }
}

/// Several lines.
///
/// Built from a string, it splits on `\n` (and `\r\n`), and a trailing
/// newline leaves an empty last line, as in an editor.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Text<'a> {
    /// The lines, top to bottom.
    pub lines: Vec<Line<'a>>,
    /// A style applied under every line.
    pub style: Style,
}

impl<'a> Text<'a> {
    /// Text split into lines at each newline.
    pub fn raw(content: impl Into<Cow<'a, str>>) -> Self {
        // A `\r` only counts when a `\n` follows it, so the last segment
        // keeps whatever it ends with.
        let lines = match content.into() {
            Cow::Borrowed(s) => {
                let last = s.matches('\n').count();
                s.split('\n')
                    .enumerate()
                    .map(|(i, l)| {
                        Line::raw(if i < last {
                            l.strip_suffix('\r').unwrap_or(l)
                        } else {
                            l
                        })
                    })
                    .collect()
            }
            Cow::Owned(s) => {
                let last = s.matches('\n').count();
                s.split('\n')
                    .enumerate()
                    .map(|(i, l)| {
                        let l = if i < last {
                            l.strip_suffix('\r').unwrap_or(l)
                        } else {
                            l
                        };
                        Line::raw(l.to_owned())
                    })
                    .collect()
            }
        };
        Text {
            lines,
            style: Style::new(),
        }
    }

    /// The same text with `style` layered under its lines.
    pub fn style(mut self, style: Style) -> Self {
        self.style = self.style.patch(style);
        self
    }

    /// Number of lines, before any wrapping.
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    /// Columns of the widest line.
    pub fn width(&self) -> usize {
        self.lines.iter().map(Line::width).max().unwrap_or(0)
    }

    /// The same text, owning all of it, so it can outlive what it was
    /// built from.
    pub fn into_owned(self) -> Text<'static> {
        Text {
            lines: self.lines.into_iter().map(Line::into_owned).collect(),
            style: self.style,
        }
    }
}

impl<'a> From<&'a str> for Text<'a> {
    fn from(s: &'a str) -> Self {
        Text::raw(s)
    }
}

impl From<String> for Text<'_> {
    fn from(s: String) -> Self {
        Text::raw(s)
    }
}

impl<'a> From<Line<'a>> for Text<'a> {
    fn from(line: Line<'a>) -> Self {
        Text {
            lines: vec![line],
            style: Style::new(),
        }
    }
}

impl<'a> From<Span<'a>> for Text<'a> {
    fn from(span: Span<'a>) -> Self {
        Text::from(Line::from(span))
    }
}

impl<'a> From<Vec<Line<'a>>> for Text<'a> {
    fn from(lines: Vec<Line<'a>>) -> Self {
        Text {
            lines,
            style: Style::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILY: &str = "👨‍👩‍👧‍👦";
    const FLAG: &str = "🇧🇷";

    #[test]
    fn into_owned_keeps_everything_and_drops_the_borrow() {
        let source = String::from("one\ntwo");
        let styled = Style::new().bold();
        let mut text = Text::raw(source.as_str()).style(styled);
        text.lines[1] = Line::from(vec![Span::styled(&source[4..], styled)])
            .style(styled)
            .align(HorizontalAlign::Right);
        let expected = text.clone();
        let owned: Text<'static> = text.into_owned();
        assert_eq!(owned, expected);
        assert!(
            owned
                .lines
                .iter()
                .all(|l| { l.spans.iter().all(|s| matches!(s.content, Cow::Owned(_))) })
        );
    }

    #[test]
    fn width_of_common_text() {
        assert_eq!(width("hello"), 5);
        assert_eq!(width("中文"), 4);
        assert_eq!(width("e\u{301}"), 1);
        assert_eq!(width("😀"), 2);
        assert_eq!(width(FAMILY), 2);
        assert_eq!(width(FLAG), 2);
        assert_eq!(width("❤\u{fe0f}"), 2);
        assert_eq!(width("a\tb"), 2);
        assert_eq!(width(""), 0);
    }

    #[test]
    fn truncate_never_splits_a_grapheme() {
        assert_eq!(truncate("hello", 3), "hel");
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("中文字", 3), "中");
        assert_eq!(truncate("中文字", 1), "");
        assert_eq!(truncate("e\u{301}x", 1), "e\u{301}");
        assert_eq!(truncate(FAMILY, 1), "");
        assert_eq!(truncate(FAMILY, 2), FAMILY);
        assert_eq!(truncate("ab", 0), "");
    }

    #[test]
    fn wrap_breaks_on_columns() {
        assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap("中文字", 4), vec!["中文", "字"]);
        assert_eq!(wrap("a中b", 2), vec!["a", "中", "b"]);
    }

    #[test]
    fn wrap_keeps_newlines_and_empty_lines() {
        assert_eq!(wrap("ab\n\ncd", 5), vec!["ab", "", "cd"]);
        assert_eq!(wrap("ab\r\ncd", 5), vec!["ab", "cd"]);
        assert_eq!(wrap("", 5), vec![""]);
        // A trailing newline leaves an empty last line, as in an editor.
        assert_eq!(wrap("ab\n", 5), vec!["ab", ""]);
    }

    #[test]
    fn wrap_makes_progress_when_glyph_is_wider_than_line() {
        assert_eq!(wrap("中a", 1), vec!["中", "a"]);
        assert_eq!(wrap("abc", 0), Vec::<&str>::new());
    }

    #[test]
    fn wrap_is_lossless_for_single_line_input() {
        let s = format!("ab{FAMILY}e\u{301}中{FLAG}cd");
        for cols in 1..8 {
            let joined: String = wrap(&s, cols).concat();
            assert_eq!(joined, s, "cols={cols}");
            for line in wrap(&s, cols) {
                assert!(width(line) <= cols || line.graphemes(true).count() == 1);
            }
        }
    }
}

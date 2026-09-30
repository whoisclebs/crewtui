//! Display-width aware helpers for strings.
//!
//! Terminals lay text out in columns, and a column is not a byte, a `char`
//! or even a grapheme cluster. These helpers work on grapheme clusters and
//! count columns, so wide (CJK, emoji) and zero-width (combining marks)
//! text lines up the way it does on screen.
//!
//! Control characters, including tabs, have zero width and are not drawn.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

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

#[cfg(test)]
mod tests {
    use super::*;

    const FAMILY: &str = "👨‍👩‍👧‍👦";
    const FLAG: &str = "🇧🇷";

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

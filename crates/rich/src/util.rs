use std::borrow::Cow;

use crewtui::text::width;

/// Replaces each tab with the spaces that reach the next multiple of four
/// columns. A terminal shows a tab as a jump, and CrewTUI's buffer draws
/// neither a tab nor the jump, so code indented with tabs would lose its
/// indentation.
pub(crate) fn expand_tabs(line: &str) -> Cow<'_, str> {
    if !line.contains('\t') {
        return Cow::Borrowed(line);
    }
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = 0;
    let mut buf = [0u8; 4];
    for c in line.chars() {
        if c == '\t' {
            let n = 4 - column % 4;
            out.extend(std::iter::repeat_n(' ', n));
            column += n;
        } else {
            out.push(c);
            column += width(c.encode_utf8(&mut buf));
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_jump_to_the_next_multiple_of_four_columns() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("a\tb"), "a   b");
        assert_eq!(expand_tabs("abcd\te"), "abcd    e");
        assert_eq!(expand_tabs("日\tx"), "日  x");
        assert_eq!(expand_tabs("\t\t"), "        ");
        assert!(matches!(expand_tabs("no tabs"), Cow::Borrowed(_)));
    }
}

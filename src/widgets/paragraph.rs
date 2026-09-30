use unicode_segmentation::UnicodeSegmentation;

use super::{Block, Widget};
use crate::text::{HorizontalAlign, Line, Text, grapheme_width};
use crate::{Buffer, Rect, Style};

/// How a [`Paragraph`] handles lines longer than its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Wrap {
    /// Cut the line at the edge.
    #[default]
    None,
    /// Continue on the next row at whatever character comes next.
    Char,
    /// Continue on the next row at a word boundary. The whitespace where the
    /// line breaks is dropped, and a word longer than a whole row is split
    /// by characters. Indentation is kept, unless the first word no longer
    /// fits after it, in which case the word starts the row. No-break spaces
    /// are not places to break.
    Word,
}

/// Styled text in a rectangle, with optional wrapping, alignment and scroll.
///
/// ```
/// use crewtui::widgets::{Paragraph, Widget, Wrap};
/// use crewtui::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 8, 3);
/// let mut buf = Buffer::new(area);
/// Paragraph::new("hello world foo").wrap(Wrap::Word).render(area, &mut buf);
/// assert_eq!(buf.get(0, 1).unwrap().symbol(), "w");
/// ```
///
/// Style is layered: the paragraph's, then the text's, then the line's, then
/// the span's, later ones winning where they set something.
///
/// Without wrapping, only the lines that are visible are looked at, so a
/// text of a million lines costs what fits on screen. With wrapping the
/// rows above the scroll offset have to be measured to know where it falls,
/// so the cost grows with the offset; a transcript that scrolls far should
/// keep the wrapped line counts itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph<'a> {
    text: Text<'a>,
    block: Option<Block<'a>>,
    style: Style,
    wrap: Wrap,
    align: HorizontalAlign,
    scroll: usize,
}

impl<'a> Paragraph<'a> {
    /// A paragraph of `text`: no wrapping, left aligned, unscrolled.
    pub fn new(text: impl Into<Text<'a>>) -> Self {
        Paragraph {
            text: text.into(),
            block: None,
            style: Style::new(),
            wrap: Wrap::None,
            align: HorizontalAlign::Left,
            scroll: 0,
        }
    }

    /// A block drawn around the text. The text goes in the block's inner
    /// area.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The style under everything else, applied to the whole area.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// How long lines are handled.
    pub fn wrap(mut self, wrap: Wrap) -> Self {
        self.wrap = wrap;
        self
    }

    /// Where lines sit horizontally. A line's own alignment wins.
    pub fn align(mut self, align: HorizontalAlign) -> Self {
        self.align = align;
        self
    }

    /// The number of rows to skip from the top. With wrapping, these are
    /// wrapped rows.
    pub fn scroll(mut self, rows: usize) -> Self {
        self.scroll = rows;
        self
    }
}

/// One grapheme of a line, ready to be placed.
struct Piece<'a> {
    text: &'a str,
    width: usize,
    style: Style,
    whitespace: bool,
}

/// A run of pieces that goes on one screen row.
#[derive(Debug, PartialEq, Eq)]
struct Row {
    start: usize,
    end: usize,
    width: usize,
}

#[cfg(test)]
thread_local! {
    /// How many lines were split into pieces, to check that a frame only
    /// looks at the lines it draws.
    pub(crate) static PIECES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn pieces<'a>(line: &'a Line<'_>) -> Vec<Piece<'a>> {
    #[cfg(test)]
    PIECES.with(|c| c.set(c.get() + 1));
    let mut out = Vec::new();
    for span in &line.spans {
        for g in span.content.graphemes(true) {
            let width = grapheme_width(g);
            // Control characters take no room and aren't drawn.
            if width == 0 {
                continue;
            }
            out.push(Piece {
                text: g,
                width,
                style: span.style,
                whitespace: g.chars().all(is_breaking_space),
            });
        }
    }
    out
}

/// Whitespace a line may be broken at. No-break spaces are whitespace, but
/// the text asked for them not to be a break.
fn is_breaking_space(c: char) -> bool {
    c.is_whitespace() && !matches!(c, '\u{a0}' | '\u{2007}' | '\u{202f}')
}

fn width_of(pieces: &[Piece<'_>]) -> usize {
    pieces.iter().map(|p| p.width).sum()
}

/// Splits one line into screen rows. Never returns no rows: an empty line
/// is one empty row.
fn rows(pieces: &[Piece<'_>], width: usize, wrap: Wrap) -> Vec<Row> {
    if pieces.is_empty() {
        return vec![Row {
            start: 0,
            end: 0,
            width: 0,
        }];
    }
    match wrap {
        Wrap::None => {
            let mut used = 0;
            let mut end = 0;
            for p in pieces {
                if used + p.width > width {
                    break;
                }
                used += p.width;
                end += 1;
            }
            vec![Row {
                start: 0,
                end,
                width: used,
            }]
        }
        Wrap::Char => char_rows(pieces, width),
        Wrap::Word => word_rows(pieces, width),
    }
}

fn char_rows(pieces: &[Piece<'_>], width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    for (i, p) in pieces.iter().enumerate() {
        if used + p.width > width && i > start {
            rows.push(Row {
                start,
                end: i,
                width: used,
            });
            start = i;
            used = 0;
        }
        // A glyph wider than the whole row gets a row of its own and is
        // clipped when drawn, so this always makes progress.
        used += p.width;
    }
    rows.push(Row {
        start,
        end: pieces.len(),
        width: used,
    });
    rows
}

fn word_rows(pieces: &[Piece<'_>], width: usize) -> Vec<Row> {
    let n = pieces.len();
    let mut rows = Vec::new();
    let mut start = 0;
    let mut used = 0;
    let mut i = 0;
    // A row that ends at a wrap loses its trailing whitespace.
    let trimmed = |start: usize, end: usize| {
        let mut end = end;
        while end > start && pieces[end - 1].whitespace {
            end -= 1;
        }
        Row {
            start,
            end,
            width: width_of(&pieces[start..end]),
        }
    };
    while i < n {
        let ws = pieces[i].whitespace;
        let mut j = i;
        let mut run = 0;
        while j < n && pieces[j].whitespace == ws {
            run += pieces[j].width;
            j += 1;
        }
        if ws {
            if used + run <= width {
                used += run;
                i = j;
            } else {
                // The line breaks here and this whitespace is dropped.
                if i > start {
                    rows.push(trimmed(start, i));
                }
                start = j;
                used = 0;
                i = j;
            }
        } else if used + run <= width {
            used += run;
            i = j;
        } else if i > start {
            // The row has something on it: the word starts the next one. If
            // all it has is indentation, that is dropped rather than left
            // as an empty row.
            if trimmed(start, i).end > start {
                rows.push(trimmed(start, i));
            }
            start = i;
            used = 0;
        } else {
            // A word wider than a whole row is split by characters.
            let mut k = i;
            let mut taken = 0;
            while k < j && (k == i || taken + pieces[k].width <= width) {
                taken += pieces[k].width;
                k += 1;
            }
            rows.push(Row {
                start: i,
                end: k,
                width: taken,
            });
            start = k;
            used = 0;
            i = k;
        }
    }
    if start < n || rows.is_empty() {
        rows.push(Row {
            start,
            end: n,
            width: used,
        });
    }
    rows
}

/// Draws one line on row `y` of `area` without wrapping: cut by columns and
/// placed by `align`, unless the line has an alignment of its own. `base` is
/// the style under the line's own and its spans'.
pub(crate) fn draw_line(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    line: &Line<'_>,
    base: Style,
    align: HorizontalAlign,
) {
    let pieces = pieces(line);
    let row = rows(&pieces, usize::from(area.width), Wrap::None).remove(0);
    draw_pieces(buf, area, y, line, &pieces, &row, base, align);
}

#[allow(clippy::too_many_arguments)]
fn draw_pieces(
    buf: &mut Buffer,
    area: Rect,
    y: u16,
    line: &Line<'_>,
    pieces: &[Piece<'_>],
    row: &Row,
    base: Style,
    align: HorizontalAlign,
) {
    let align = line.alignment.unwrap_or(align);
    let free = usize::from(area.width).saturating_sub(row.width);
    let offset = match align {
        HorizontalAlign::Left => 0,
        HorizontalAlign::Center => free / 2,
        HorizontalAlign::Right => free,
    };
    let base = base.patch(line.style);
    let mut x = area.x.saturating_add(offset as u16);
    let right = area.right();
    for p in &pieces[row.start..row.end] {
        if usize::from(x) + p.width > usize::from(right) {
            break;
        }
        x = buf.set_string(x, y, p.text, base.patch(p.style));
    }
}

/// How many screen rows `line` takes at `width` columns.
pub(crate) fn line_rows(line: &Line<'_>, width: usize, wrap: Wrap) -> usize {
    if wrap == Wrap::None {
        return 1;
    }
    rows(&pieces(line), width, wrap).len()
}

/// Draws `text` from its row `skip` down as far as `area` reaches, and says
/// how many rows it drew. `base` goes under the text's own style.
pub(crate) fn draw_text(
    buf: &mut Buffer,
    area: Rect,
    text: &Text<'_>,
    base: Style,
    wrap: Wrap,
    align: HorizontalAlign,
    skip: usize,
) -> usize {
    draw_lines(
        buf,
        area,
        &text.lines,
        base.patch(text.style),
        wrap,
        align,
        skip,
    )
}

/// Like `draw_text` for a slice of lines, with `base` already including the
/// text's style. The lines before the slice are not looked at.
pub(crate) fn draw_lines(
    buf: &mut Buffer,
    area: Rect,
    lines: &[Line<'_>],
    base: Style,
    wrap: Wrap,
    align: HorizontalAlign,
    skip: usize,
) -> usize {
    let width = usize::from(area.width);
    let height = usize::from(area.height);
    let mut drawn = 0;

    if wrap == Wrap::None {
        // One row per line, so the scroll offset is a jump, not a walk.
        for line in lines.iter().skip(skip).take(height) {
            let pieces = pieces(line);
            let row = rows(&pieces, width, Wrap::None).remove(0);
            draw_pieces(
                buf,
                area,
                area.y + drawn as u16,
                line,
                &pieces,
                &row,
                base,
                align,
            );
            drawn += 1;
        }
        return drawn;
    }

    let mut skip = skip;
    for line in lines {
        let pieces = pieces(line);
        for row in rows(&pieces, width, wrap) {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            draw_pieces(
                buf,
                area,
                area.y + drawn as u16,
                line,
                &pieces,
                &row,
                base,
                align,
            );
            drawn += 1;
            if drawn >= height {
                return drawn;
            }
        }
    }
    drawn
}

impl Widget for Paragraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        let area = match &self.block {
            Some(block) => {
                let inner = block.inner(area);
                block.clone().render(area, buf);
                inner
            }
            None => area,
        };
        if area.is_empty() {
            return;
        }
        buf.set_style(area, self.style);
        draw_text(
            buf,
            area,
            &self.text,
            self.style,
            self.wrap,
            self.align,
            self.scroll,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Span;
    use crate::{Color, Frame};

    /// The rows of `buf` as text, one symbol per cell.
    fn rows_of(buf: &Buffer) -> Vec<String> {
        let a = buf.area();
        (a.y..a.bottom())
            .map(|y| {
                (a.x..a.right())
                    .map(|x| buf.get(x, y).unwrap().symbol())
                    .collect()
            })
            .collect()
    }

    fn draw(p: Paragraph<'_>, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(area);
        p.render(area, &mut buf);
        rows_of(&buf)
    }

    fn wrapped(text: &str, mode: Wrap, w: u16, h: u16) -> Vec<String> {
        draw(Paragraph::new(text).wrap(mode), w, h)
    }

    #[test]
    fn text_is_cut_at_the_edge_by_columns_not_bytes() {
        assert_eq!(draw(Paragraph::new("hello world"), 5, 1), ["hello"]);
        // A wide glyph that would straddle the edge is left out.
        assert_eq!(draw(Paragraph::new("ab中文"), 3, 1), ["ab "]);
        assert_eq!(draw(Paragraph::new("中文"), 4, 1), ["中文"]);
    }

    #[test]
    fn lines_go_on_their_own_rows_and_empty_lines_are_kept() {
        assert_eq!(
            draw(Paragraph::new("a\n\nb"), 3, 4),
            ["a  ", "   ", "b  ", "   "]
        );
    }

    #[test]
    fn alignment_places_each_line_and_a_lines_own_alignment_wins() {
        let text = Text::from(vec![
            Line::raw("ab"),
            Line::raw("ab").align(HorizontalAlign::Right),
        ]);
        assert_eq!(
            draw(
                Paragraph::new(text.clone()).align(HorizontalAlign::Center),
                6,
                2
            ),
            ["  ab  ", "    ab"]
        );
        assert_eq!(
            draw(Paragraph::new(text).align(HorizontalAlign::Right), 5, 2),
            ["   ab", "   ab"]
        );
        // An odd extra cell goes to the right.
        assert_eq!(
            draw(Paragraph::new("ab").align(HorizontalAlign::Center), 5, 1),
            [" ab  "]
        );
    }

    #[test]
    fn scroll_skips_rows_from_the_top() {
        assert_eq!(
            draw(Paragraph::new("1\n2\n3\n4").scroll(2), 2, 3),
            ["3 ", "4 ", "  "]
        );
        assert_eq!(draw(Paragraph::new("1\n2").scroll(10), 2, 2), ["  ", "  "]);
    }

    #[test]
    fn it_draws_inside_its_area_and_leaves_the_rest_alone() {
        let mut buf = Buffer::new(Rect::new(0, 0, 8, 4));
        buf.set_string(0, 0, "........", Style::new());
        buf.set_string(0, 3, "........", Style::new());
        Paragraph::new("abcdefgh\nxyz").render(Rect::new(2, 1, 3, 2), &mut buf);
        assert_eq!(
            rows_of(&buf),
            ["........", "  abc   ", "  xyz   ", "........"]
        );
    }

    #[test]
    fn empty_areas_and_areas_beyond_the_buffer_are_fine() {
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 2));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(1, 1, 0, 3),
            Rect::new(9, 9, 5, 5),
            Rect::new(3, 1, 10, 10),
        ] {
            Paragraph::new("text")
                .wrap(Wrap::Word)
                .render(area, &mut buf);
        }
        assert_eq!(rows_of(&buf), ["    ", "   t"]);
    }

    #[test]
    fn styles_layer_from_the_paragraph_down_to_the_span() {
        let text = Text::from(vec![
            Line::from(vec![
                Span::raw("a"),
                Span::styled("b", Style::new().fg(Color::Red)),
            ])
            .style(Style::new().bg(Color::Blue)),
        ])
        .style(Style::new().fg(Color::Green).bold());
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::new(area);
        Paragraph::new(text)
            .style(Style::new().italic())
            .render(area, &mut buf);
        let a = buf.get(0, 0).unwrap().style();
        assert_eq!(
            a,
            Style::new()
                .italic()
                .fg(Color::Green)
                .bold()
                .bg(Color::Blue)
        );
        // The span's own foreground wins over the text's.
        assert_eq!(buf.get(1, 0).unwrap().style().fg, Some(Color::Red));
        // The paragraph style covers the cells the text doesn't reach.
        assert_eq!(buf.get(3, 0).unwrap().style(), Style::new().italic());
    }

    #[test]
    fn char_wrap_breaks_anywhere_and_never_inside_a_grapheme() {
        assert_eq!(wrapped("abcdefgh", Wrap::Char, 3, 3), ["abc", "def", "gh "]);
        assert_eq!(wrapped("中文字", Wrap::Char, 3, 3), ["中 ", "文 ", "字 "]);
        assert_eq!(wrapped("a😀b", Wrap::Char, 2, 3), ["a ", "😀", "b "]);
        assert_eq!(wrapped("e\u{301}xy", Wrap::Char, 2, 2), ["e\u{301}x", "y "]);
    }

    #[test]
    fn a_glyph_wider_than_the_area_does_not_stall_the_wrap() {
        for mode in [Wrap::Char, Wrap::Word] {
            let rows = wrapped("中a", mode, 1, 3);
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[1], "a", "{mode:?}");
        }
    }

    #[test]
    fn word_wrap_breaks_between_words_and_drops_the_whitespace_it_breaks_at() {
        assert_eq!(
            wrapped("hello world foo", Wrap::Word, 8, 4),
            ["hello   ", "world   ", "foo     ", "        "]
        );
        assert_eq!(
            wrapped("aa bb cc", Wrap::Word, 5, 3),
            ["aa bb", "cc   ", "     "]
        );
    }

    #[test]
    fn word_wrap_splits_a_word_longer_than_the_row() {
        assert_eq!(
            wrapped("abcdefghij", Wrap::Word, 4, 3),
            ["abcd", "efgh", "ij  "]
        );
        assert_eq!(
            wrapped("ab abcdefgh", Wrap::Word, 4, 4),
            ["ab  ", "abcd", "efgh", "    "]
        );
    }

    #[test]
    fn word_wrap_keeps_indentation_and_inner_spacing() {
        assert_eq!(wrapped("  ab cd", Wrap::Word, 5, 2), ["  ab ", "cd   "]);
        assert_eq!(wrapped("a   b", Wrap::Word, 6, 1), ["a   b "]);
    }

    #[test]
    fn indentation_that_leaves_no_room_for_the_first_word_is_dropped_not_left_as_a_row() {
        assert_eq!(wrapped("  hello", Wrap::Word, 5, 2), ["hello", "     "]);
        assert_eq!(
            wrapped("   abcdefgh", Wrap::Word, 4, 3),
            ["abcd", "efgh", "    "]
        );
        assert_eq!(wrapped("  hi", Wrap::Word, 5, 2), ["  hi ", "     "]);
    }

    #[test]
    fn a_no_break_space_is_not_a_place_to_break() {
        assert_eq!(
            wrapped("a\u{a0}bcde", Wrap::Word, 4, 3),
            ["a\u{a0}bc", "de  ", "    "]
        );
        assert_eq!(wrapped("a bcde", Wrap::Word, 4, 2), ["a   ", "bcde"]);
    }

    #[test]
    fn word_wrap_handles_cjk_and_mixed_width_words() {
        assert_eq!(
            wrapped("中文 字 abc", Wrap::Word, 6, 3),
            ["中文  ", "字 abc", "      "]
        );
        assert_eq!(
            wrapped("中文 字", Wrap::Word, 5, 3),
            ["中文 ", "字   ", "     "]
        );
    }

    #[test]
    fn wrapped_rows_are_aligned_without_their_trailing_whitespace() {
        let p = Paragraph::new("aa bb cc")
            .wrap(Wrap::Word)
            .align(HorizontalAlign::Right);
        assert_eq!(draw(p, 4, 3), ["  aa", "  bb", "  cc"]);
    }

    #[test]
    fn wrapping_keeps_empty_lines_and_blank_only_lines() {
        for mode in [Wrap::Char, Wrap::Word] {
            assert_eq!(
                wrapped("a\n\n   \nb", mode, 4, 5),
                ["a   ", "    ", "    ", "b   ", "    "],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn scroll_counts_wrapped_rows() {
        assert_eq!(
            draw(Paragraph::new("abcdef").wrap(Wrap::Char).scroll(1), 3, 2),
            ["def", "   "]
        );
        assert_eq!(
            draw(Paragraph::new("ab cd\nef").wrap(Wrap::Word).scroll(1), 2, 3),
            ["cd", "ef", "  "]
        );
    }

    #[test]
    fn control_characters_are_not_drawn_and_take_no_room() {
        assert_eq!(draw(Paragraph::new("a\tb\u{1b}c"), 5, 1), ["abc  "]);
        assert_eq!(wrapped("a\tb\u{1b}c", Wrap::Char, 2, 2), ["ab", "c "]);
    }

    #[test]
    fn a_text_of_a_million_lines_is_not_measured_in_full() {
        let text: String = (0..1_000_000).map(|i| format!("line {i}\n")).collect();
        let text = Text::raw(text);
        let started = std::time::Instant::now();
        // No wrap: the scroll offset is a jump.
        let rows = draw(Paragraph::new(text.clone()).scroll(999_990), 12, 3);
        assert_eq!(rows[0].trim_end(), "line 999990");
        // Wrap from the top: only the rows that show are laid out.
        let rows = draw(Paragraph::new(text).wrap(Wrap::Word), 12, 3);
        assert_eq!(rows[2].trim_end(), "line 2");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(20),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn frame_render_widget_clips_to_the_frame() {
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::new(area);
        let mut frame = Frame::new(&mut buf);
        frame.render_widget("abcdefgh", Rect::new(2, 0, 20, 20));
        frame.render_widget(Text::from("xy\nz"), Rect::new(0, 1, 5, 1));
        assert_eq!(rows_of(&buf), ["  abc", "xy   "]);
    }

    #[test]
    fn span_line_and_text_are_widgets_too() {
        let area = Rect::new(0, 0, 4, 3);
        let mut buf = Buffer::new(area);
        Span::styled("ab", Style::new().bold()).render(Rect::new(0, 0, 4, 1), &mut buf);
        Line::raw("cd").render(Rect::new(0, 1, 4, 1), &mut buf);
        String::from("ef").render(Rect::new(0, 2, 4, 1), &mut buf);
        assert_eq!(rows_of(&buf), ["ab  ", "cd  ", "ef  "]);
        assert_eq!(buf.get(0, 0).unwrap().style(), Style::new().bold());
    }

    #[test]
    fn text_from_a_string_splits_lines() {
        let t = Text::from("a\r\nb\n");
        assert_eq!(t.height(), 3);
        assert_eq!(t.lines[1], Line::raw("b"));
        assert_eq!(t.lines[2], Line::raw(""));
        // A `\r` only goes with the `\n` after it.
        assert_eq!(Text::from("a\r").lines, vec![Line::raw("a\r")]);
        assert_eq!(
            Text::from(String::from("a\r\nb\r")).lines,
            vec![Line::raw("a"), Line::raw("b\r")]
        );
        assert_eq!(Text::from("中a").width(), 3);
        assert_eq!(
            Line::from(vec![Span::raw("ab"), Span::raw("中")]).width(),
            4
        );
    }
}

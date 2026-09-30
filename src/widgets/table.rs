use std::cell::Cell;

use super::paragraph::draw_line;
use super::{Block, StatefulWidget, Widget, offset_for};
use crate::text::{HorizontalAlign, Text, width};
use crate::{Buffer, Constraint, Layout, Rect, Style};

/// One row of a [`Table`]: a text for each column.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row<'a> {
    cells: Vec<Text<'a>>,
    style: Style,
}

impl<'a> Row<'a> {
    /// A row with one cell for each item. A cell with several lines makes
    /// the row that many rows tall.
    pub fn new<C>(cells: impl IntoIterator<Item = C>) -> Self
    where
        C: Into<Text<'a>>,
    {
        Row {
            cells: cells.into_iter().map(Into::into).collect(),
            style: Style::new(),
        }
    }

    /// The style under the whole row.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Screen rows the row takes: its tallest cell, and at least one.
    pub fn height(&self) -> usize {
        self.cells
            .iter()
            .map(Text::height)
            .max()
            .unwrap_or(0)
            .max(1)
    }
}

/// Which row is selected and which is at the top: the state of a [`Table`].
///
/// It works like [`ListState`](super::ListState): the app owns it and
/// changes the selection in `update`, and the offset is worked out while
/// drawing, moved just far enough to keep the selected row in view, and kept
/// in a `Cell` so that it is still there next frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TableState {
    selected: Option<usize>,
    offset: Cell<usize>,
}

impl TableState {
    /// A state with nothing selected, scrolled to the top.
    pub fn new() -> Self {
        TableState::default()
    }

    /// The selected row's index.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Selects a row, or nothing.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// The index of the row at the top of the last frame drawn.
    pub fn offset(&self) -> usize {
        self.offset.get()
    }

    /// Selects the next of `len` rows, stopping at the last, or the first
    /// when nothing was selected.
    pub fn select_next(&mut self, len: usize) {
        self.selected = match (self.selected, len) {
            (_, 0) => None,
            (None, _) => Some(0),
            (Some(i), len) => Some((i + 1).min(len - 1)),
        };
    }

    /// Selects the previous row, stopping at the first, or the last of `len`
    /// when nothing was selected.
    pub fn select_previous(&mut self, len: usize) {
        self.selected = match (self.selected, len) {
            (_, 0) => None,
            (None, len) => Some(len - 1),
            (Some(i), len) => Some(i.saturating_sub(1).min(len - 1)),
        };
    }
}

/// Rows in columns, with an optional header and a selected row.
///
/// The columns are sized by [`Constraint`]s, the same ones a [`Layout`]
/// takes, and `column_gap` cells are left between them. Text that is wider
/// than its column is cut off at the column's edge. The header stays at the
/// top while the rows scroll under it, and the selected row is kept in view.
///
/// ```
/// use crewtui::widgets::{Row, StatefulWidget, Table, TableState};
/// use crewtui::{Buffer, Constraint, Rect};
///
/// let area = Rect::new(0, 0, 14, 3);
/// let mut buf = Buffer::new(area);
/// let mut state = TableState::new();
/// state.select(Some(0));
/// Table::new(
///     [Row::new(["src", "12"]), Row::new(["docs", "3"])],
///     [Constraint::Fill(1), Constraint::Fixed(3)],
/// )
/// .header(Row::new(["dir", "n"]))
/// .render(area, &mut buf, &state);
/// assert_eq!(buf.get(2, 1).unwrap().symbol(), "s");
/// ```
///
/// Only the rows that are shown are drawn. The rows themselves are built by
/// the caller each frame, like the items of a [`List`](super::List).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table<'a> {
    rows: Vec<Row<'a>>,
    header: Option<Row<'a>>,
    widths: Vec<Constraint>,
    column_gap: u16,
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
    highlight_symbol: &'a str,
}

impl<'a> Table<'a> {
    /// A table of `rows` in columns as wide as `widths` say. A row with
    /// fewer cells than there are columns leaves the rest blank, and cells
    /// past the last column are not drawn.
    pub fn new(
        rows: impl IntoIterator<Item = Row<'a>>,
        widths: impl IntoIterator<Item = Constraint>,
    ) -> Self {
        Table {
            rows: rows.into_iter().collect(),
            header: None,
            widths: widths.into_iter().collect(),
            column_gap: 1,
            block: None,
            style: Style::new(),
            highlight_style: Style::new(),
            highlight_symbol: "> ",
        }
    }

    /// A header row, drawn above the rows and always in view.
    pub fn header(mut self, header: Row<'a>) -> Self {
        self.header = Some(header);
        self
    }

    /// Cells left between two columns. One by default.
    pub fn column_gap(mut self, gap: u16) -> Self {
        self.column_gap = gap;
        self
    }

    /// A block drawn around the table.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The style under everything in the table.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The style of the selected row, over the row's own.
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    /// What is drawn before the selected row; other rows and the header get
    /// blank space of the same width, so nothing shifts when the selection
    /// moves.
    pub fn highlight_symbol(mut self, symbol: &'a str) -> Self {
        self.highlight_symbol = symbol;
        self
    }

    /// Number of rows, not counting the header.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// True when there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Draws line `line` of each cell of `row` in its column, on screen row `y`.
fn draw_row_line(
    buf: &mut Buffer,
    columns: &[Rect],
    y: u16,
    row: &Row<'_>,
    line: usize,
    style: Style,
) {
    for (column, cell) in columns.iter().zip(&row.cells) {
        if let Some(text) = cell.lines.get(line) {
            draw_line(
                buf,
                *column,
                y,
                text,
                style.patch(cell.style),
                HorizontalAlign::Left,
            );
        }
    }
}

impl StatefulWidget for Table<'_> {
    type State = TableState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &TableState) {
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

        let symbol_width = width(self.highlight_symbol).min(usize::from(area.width)) as u16;
        let content = Rect {
            x: area.x.saturating_add(symbol_width),
            width: area.width - symbol_width,
            ..area
        };
        let columns = Layout::row()
            .constraints(self.widths.iter().copied())
            .gap(self.column_gap)
            .split(content);

        let mut y = area.y;
        if let Some(header) = &self.header {
            for line in 0..header.height() {
                if y >= area.bottom() {
                    break;
                }
                let row_area = Rect {
                    y,
                    height: 1,
                    ..area
                };
                buf.set_style(row_area, self.style.patch(header.style));
                draw_row_line(
                    buf,
                    &columns,
                    y,
                    header,
                    line,
                    self.style.patch(header.style),
                );
                y += 1;
            }
        }
        let body = Rect {
            y,
            height: area.bottom() - y,
            ..area
        };
        if self.rows.is_empty() {
            state.offset.set(0);
            return;
        }
        if body.is_empty() {
            // The header takes the whole area. Keep the scroll position for
            // when there is room again.
            return;
        }

        let last = self.rows.len() - 1;
        // A selection past the end counts as the last row.
        let selected = state.selected.map(|s| s.min(last));
        let mut offset = state.offset.get().min(last);
        if let Some(selected) = selected {
            if selected < offset {
                offset = selected;
            } else {
                offset = offset.max(offset_for(
                    |i| self.rows[i].height(),
                    selected,
                    usize::from(body.height),
                ));
            }
        }
        state.offset.set(offset);

        'rows: for (i, row) in self.rows.iter().enumerate().skip(offset) {
            let selected = selected == Some(i);
            let style = if selected {
                self.style.patch(row.style).patch(self.highlight_style)
            } else {
                self.style.patch(row.style)
            };
            for line in 0..row.height() {
                if y >= body.bottom() {
                    break 'rows;
                }
                buf.set_style(
                    Rect {
                        y,
                        height: 1,
                        ..area
                    },
                    style,
                );
                if symbol_width > 0 && selected && line == 0 {
                    let symbol =
                        crate::text::truncate(self.highlight_symbol, usize::from(symbol_width));
                    buf.set_string(area.x, y, symbol, style);
                }
                draw_row_line(buf, &columns, y, row, line, style);
                y += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Line;
    use crate::{Color, Frame};

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

    fn draw(table: Table<'_>, state: &TableState, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(area);
        table.render(area, &mut buf, state);
        rows_of(&buf)
    }

    fn files(n: usize) -> Vec<Row<'static>> {
        (0..n)
            .map(|i| Row::new([format!("f{i}"), format!("{}", i * 10)]))
            .collect()
    }

    const WIDTHS: [Constraint; 2] = [Constraint::Fixed(4), Constraint::Fill(1)];

    #[test]
    fn columns_follow_their_constraints_with_a_gap_and_room_for_the_symbol() {
        let state = TableState::new();
        assert_eq!(
            draw(Table::new(files(2), WIDTHS), &state, 12, 3),
            ["  f0   0     ", "  f1   10    ", "             "][..]
                .iter()
                .map(|r| &r[..12])
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn the_header_stays_while_the_rows_scroll_under_it() {
        let mut state = TableState::new();
        state.select(Some(9));
        let out = draw(
            Table::new(files(10), WIDTHS).header(Row::new(["name", "size"])),
            &state,
            12,
            4,
        );
        assert_eq!(out[0], "  name size ");
        assert_eq!(out[3], "> f9   90   ");
        assert_eq!(state.offset(), 7);
        state.select(Some(0));
        let out = draw(
            Table::new(files(10), WIDTHS).header(Row::new(["name", "size"])),
            &state,
            12,
            4,
        );
        assert_eq!(out[0], "  name size ");
        assert_eq!(out[1], "> f0   0    ");
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn a_cell_wider_than_its_column_is_cut_at_the_edge() {
        let state = TableState::new();
        let out = draw(
            Table::new(
                [Row::new(["abcdefgh", "xy"])],
                [Constraint::Fixed(3), Constraint::Fill(1)],
            )
            .highlight_symbol(""),
            &state,
            8,
            1,
        );
        assert_eq!(out, ["abc xy   "[..8].to_owned()]);
    }

    #[test]
    fn wide_glyphs_are_cut_on_a_glyph_boundary() {
        let state = TableState::new();
        let out = draw(
            Table::new(
                [Row::new(["日本語", "x"])],
                [Constraint::Fixed(5), Constraint::Fill(1)],
            )
            .highlight_symbol(""),
            &state,
            9,
            1,
        );
        // Two wide glyphs fit in five columns, the third doesn't.
        assert_eq!(out, ["日本  x  "]);
    }

    #[test]
    fn a_row_with_several_lines_takes_that_many_rows_and_is_kept_whole() {
        let mut state = TableState::new();
        let rows = vec![
            Row::new(["a", "1"]),
            Row::new([Text::from("b\nb2"), Text::from("2")]),
            Row::new(["c", "3"]),
        ];
        state.select(Some(1));
        let out = draw(Table::new(rows.clone(), WIDTHS), &state, 10, 3);
        assert_eq!(out, ["  a    1  ", "> b    2  ", "  b2      "]);
        // Selecting the last one scrolls the tall row out, not half of it.
        state.select(Some(2));
        let out = draw(Table::new(rows, WIDTHS), &state, 10, 2);
        assert_eq!(state.offset(), 2);
        assert!(out[0].starts_with("> c"), "{out:?}");
    }

    #[test]
    fn missing_cells_are_blank_and_extra_cells_are_not_drawn() {
        let state = TableState::new();
        let out = draw(
            Table::new([Row::new(["a"]), Row::new(["b", "2", "extra"])], WIDTHS)
                .highlight_symbol(""),
            &state,
            10,
            2,
        );
        assert_eq!(out, ["a         ", "b    2    "]);
    }

    #[test]
    fn the_selected_row_gets_the_highlight_style_and_others_keep_theirs() {
        let area = Rect::new(0, 0, 12, 3);
        let mut buf = Buffer::new(area);
        let mut state = TableState::new();
        state.select(Some(1));
        Table::new(
            [
                Row::new(["a", "1"]),
                Row::new(["b", "2"]),
                Row::new(["c", "3"]).style(Style::new().fg(Color::Green)),
            ],
            WIDTHS,
        )
        .highlight_style(Style::new().bg(Color::Blue))
        .render(area, &mut buf, &state);
        assert_eq!(buf.get(0, 1).unwrap().style().bg, Some(Color::Blue));
        assert_eq!(buf.get(11, 1).unwrap().style().bg, Some(Color::Blue));
        assert_eq!(buf.get(0, 0).unwrap().style().bg, None);
        assert_eq!(buf.get(2, 2).unwrap().style().fg, Some(Color::Green));
    }

    #[test]
    fn the_header_takes_the_style_of_its_row() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buf = Buffer::new(area);
        Table::new(files(1), WIDTHS)
            .header(Row::new(["n", "s"]).style(Style::new().bold()))
            .render(area, &mut buf, &TableState::new());
        assert!(
            buf.get(2, 0)
                .unwrap()
                .style()
                .modifiers
                .contains(crate::Modifier::BOLD)
        );
        assert!(
            !buf.get(2, 1)
                .unwrap()
                .style()
                .modifiers
                .contains(crate::Modifier::BOLD)
        );
    }

    #[test]
    fn a_block_shrinks_the_area() {
        let state = TableState::new();
        let out = draw(
            Table::new(files(1), WIDTHS)
                .block(Block::bordered())
                .highlight_symbol(""),
            &state,
            10,
            3,
        );
        assert_eq!(out[1], "│f0   0  │");
    }

    #[test]
    fn a_selection_past_the_end_counts_as_the_last_row() {
        let mut state = TableState::new();
        state.select(Some(50));
        let out = draw(Table::new(files(3), WIDTHS), &state, 10, 5);
        assert!(out[2].starts_with("> f2"), "{out:?}");
    }

    #[test]
    fn squeezing_the_table_to_nothing_keeps_the_scroll_position() {
        let mut state = TableState::new();
        state.select(Some(15));
        let table = || Table::new(files(20), WIDTHS).header(Row::new(["n", "s"]));
        draw(table(), &state, 12, 6);
        assert_eq!(state.offset(), 11);
        state.select(Some(12));
        draw(table(), &state, 12, 6);
        assert_eq!(state.offset(), 11);
        // Only the header fits, then the table comes back.
        draw(table(), &state, 12, 1);
        draw(table(), &state, 12, 6);
        assert_eq!(state.offset(), 11);
    }

    #[test]
    fn a_header_taller_than_the_area_leaves_no_rows_and_does_not_panic() {
        let mut state = TableState::new();
        state.select(Some(3));
        let out = draw(
            Table::new(files(5), WIDTHS).header(Row::new([Text::from("a\nb\nc"), Text::from("x")])),
            &state,
            10,
            2,
        );
        assert_eq!(out.len(), 2);
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn degenerate_areas_and_tables_draw_nothing_and_do_not_panic() {
        let state = TableState::new();
        for (w, h) in [(0, 0), (1, 1), (2, 0), (0, 3), (3, 3)] {
            draw(
                Table::new(files(3), WIDTHS).header(Row::new(["a", "b"])),
                &state,
                w,
                h,
            );
        }
        let none = Table::new(Vec::<Row<'_>>::new(), WIDTHS);
        assert!(none.is_empty());
        assert_eq!(draw(none, &state, 6, 2), ["      ", "      "]);
        // No columns at all.
        draw(Table::new(files(2), []), &state, 6, 2);
        // A symbol wider than the area.
        draw(
            Table::new(files(2), WIDTHS).highlight_symbol("=========="),
            &state,
            4,
            2,
        );
    }

    #[test]
    fn select_next_and_previous_stop_at_the_ends() {
        let mut s = TableState::new();
        s.select_next(3);
        assert_eq!(s.selected(), Some(0));
        s.select_next(3);
        s.select_next(3);
        s.select_next(3);
        assert_eq!(s.selected(), Some(2));
        s.select_previous(3);
        s.select_previous(3);
        s.select_previous(3);
        assert_eq!(s.selected(), Some(0));
        s.select(None);
        s.select_previous(3);
        assert_eq!(s.selected(), Some(2));
        s.select_next(0);
        assert_eq!(s.selected(), None);
    }

    #[test]
    fn it_draws_through_a_frame() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buf = Buffer::new(area);
        let mut frame = Frame::new(&mut buf);
        let state = TableState::new();
        frame.render_stateful_widget(Table::new(files(2), WIDTHS), area, &state);
        assert_eq!(buf.get(2, 0).unwrap().symbol(), "f");
    }

    #[test]
    fn a_line_from_text_keeps_its_own_style_over_the_row_style() {
        let area = Rect::new(0, 0, 12, 1);
        let mut buf = Buffer::new(area);
        let cell = Text::from(Line::from(crate::text::Span::styled(
            "x",
            Style::new().fg(Color::Red),
        )));
        Table::new(
            [Row::new([cell]).style(Style::new().fg(Color::Green))],
            WIDTHS,
        )
        .highlight_symbol("")
        .render(area, &mut buf, &TableState::new());
        assert_eq!(buf.get(0, 0).unwrap().style().fg, Some(Color::Red));
    }
}

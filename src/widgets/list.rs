use std::cell::Cell;

use super::paragraph::draw_line;
use super::{Block, StatefulWidget, Widget, offset_for};
use crate::text::{HorizontalAlign, Text, width};
use crate::{Buffer, Rect, Style};

/// One entry of a [`List`]: some text, one or more lines tall.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListItem<'a> {
    content: Text<'a>,
    style: Style,
}

impl<'a> ListItem<'a> {
    /// An item showing `content`. Each line of the text is a row.
    pub fn new(content: impl Into<Text<'a>>) -> Self {
        ListItem {
            content: content.into(),
            style: Style::new(),
        }
    }

    /// The style under the item's text.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Rows the item takes: at least one.
    pub fn height(&self) -> usize {
        self.content.height().max(1)
    }
}

impl<'a, T: Into<Text<'a>>> From<T> for ListItem<'a> {
    fn from(content: T) -> Self {
        ListItem::new(content)
    }
}

/// Which item is selected and which is at the top: the state of a [`List`].
///
/// The app owns it. The selection is changed in `update`. The offset is
/// worked out while drawing, moved just far enough to keep the selected item
/// in view, and kept in a `Cell` so that drawing from `App::view`, which only
/// has `&self`, still remembers it for the next frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ListState {
    selected: Option<usize>,
    offset: Cell<usize>,
}

impl ListState {
    /// A state with nothing selected, scrolled to the top.
    pub fn new() -> Self {
        ListState::default()
    }

    /// The selected item's index.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Selects an item, or nothing.
    pub fn select(&mut self, index: Option<usize>) {
        self.selected = index;
    }

    /// The index of the item at the top of the last frame drawn.
    pub fn offset(&self) -> usize {
        self.offset.get()
    }

    /// Selects the next item of `len`, stopping at the last, or the first
    /// when nothing was selected.
    pub fn select_next(&mut self, len: usize) {
        self.selected = match (self.selected, len) {
            (_, 0) => None,
            (None, _) => Some(0),
            (Some(i), len) => Some((i + 1).min(len - 1)),
        };
    }

    /// Selects the previous item, stopping at the first, or the last of
    /// `len` when nothing was selected.
    pub fn select_previous(&mut self, len: usize) {
        self.selected = match (self.selected, len) {
            (_, 0) => None,
            (None, len) => Some(len - 1),
            (Some(i), len) => Some(i.saturating_sub(1).min(len - 1)),
        };
    }
}

/// A scrolling list of items with an optional selection.
///
/// ```
/// use crewtui::widgets::{List, ListState, StatefulWidget};
/// use crewtui::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 12, 3);
/// let mut buf = Buffer::new(area);
/// let mut state = ListState::new();
/// state.select(Some(4));
/// List::new(["a", "b", "c", "d", "e", "f"]).render(area, &mut buf, &state);
/// assert_eq!(state.offset(), 2); // scrolled so item 4 is in view
/// ```
///
/// Only the visible items are drawn, and finding the offset takes time
/// proportional to the number of rows shown, not to the length of the list.
/// The items themselves are still built by the caller each frame; for a very
/// long history that scrolls, use the history widget, which keeps its own
/// storage.
///
/// An item of several lines takes that many rows. The selected item is kept
/// entirely in view when it fits, and shown from its top when it doesn't.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List<'a> {
    items: Vec<ListItem<'a>>,
    block: Option<Block<'a>>,
    style: Style,
    highlight_style: Style,
    highlight_symbol: &'a str,
}

impl<'a> List<'a> {
    /// A list of `items`.
    pub fn new<I>(items: impl IntoIterator<Item = I>) -> Self
    where
        I: Into<ListItem<'a>>,
    {
        List {
            items: items.into_iter().map(Into::into).collect(),
            block: None,
            style: Style::new(),
            highlight_style: Style::new(),
            highlight_symbol: "> ",
        }
    }

    /// A block drawn around the list.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The style under everything in the list.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The style of the selected item's rows, over the item's own.
    pub fn highlight_style(mut self, style: Style) -> Self {
        self.highlight_style = style;
        self
    }

    /// What is drawn before the selected item; other items get blank space
    /// of the same width, so nothing shifts when the selection moves. The
    /// symbol is borrowed for the frame, so it can be a `&String` from the
    /// app's state.
    pub fn highlight_symbol(mut self, symbol: &'a str) -> Self {
        self.highlight_symbol = symbol;
        self
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when there are no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

impl StatefulWidget for List<'_> {
    type State = ListState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &ListState) {
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

        if self.items.is_empty() {
            state.offset.set(0);
            return;
        }
        let last = self.items.len() - 1;
        // A selection past the end counts as the last item.
        let selected = state.selected.map(|s| s.min(last));
        let rows = usize::from(area.height);
        let mut offset = state.offset.get().min(last);
        if let Some(selected) = selected {
            if selected < offset {
                offset = selected;
            } else {
                // Move down only as far as needed, never back up past where
                // the list already is.
                offset = offset.max(offset_for(|i| self.items[i].height(), selected, rows));
            }
        }
        state.offset.set(offset);

        let symbol_width = width(self.highlight_symbol).min(usize::from(area.width)) as u16;
        let content = Rect {
            x: area.x.saturating_add(symbol_width),
            width: area.width - symbol_width,
            ..area
        };
        let mut y = area.y;
        'items: for (i, item) in self.items.iter().enumerate().skip(offset) {
            let selected = selected == Some(i);
            let style = if selected {
                self.style.patch(item.style).patch(self.highlight_style)
            } else {
                self.style.patch(item.style)
            };
            let lines = item.content.lines.len().max(1);
            for row in 0..lines {
                if y >= area.bottom() {
                    break 'items;
                }
                let row_area = Rect {
                    y,
                    height: 1,
                    ..area
                };
                buf.set_style(row_area, style);
                // Other rows keep blank cells of the symbol's width, so
                // nothing shifts when the selection moves.
                if symbol_width > 0 && selected && row == 0 {
                    let symbol =
                        crate::text::truncate(self.highlight_symbol, usize::from(symbol_width));
                    buf.set_string(area.x, y, symbol, style);
                }
                if let Some(line) = item.content.lines.get(row) {
                    draw_line(
                        buf,
                        content,
                        y,
                        line,
                        style.patch(item.content.style),
                        HorizontalAlign::Left,
                    );
                }
                y += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Line, Span};
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

    fn draw(list: List<'_>, state: &ListState, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(area);
        list.render(area, &mut buf, state);
        rows_of(&buf)
    }

    fn items(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("item {i}")).collect()
    }

    #[test]
    fn items_are_drawn_from_the_top_with_room_for_the_highlight_symbol() {
        let state = ListState::new();
        assert_eq!(
            draw(List::new(["a", "b", "c"]), &state, 6, 4),
            ["  a   ", "  b   ", "  c   ", "      "]
        );
    }

    #[test]
    fn the_selected_item_gets_the_symbol_and_the_highlight_style_on_all_its_rows() {
        let area = Rect::new(0, 0, 8, 4);
        let mut buf = Buffer::new(area);
        let mut state = ListState::new();
        state.select(Some(1));
        let two_lines = ListItem::new("x\ny");
        List::new([ListItem::new("a"), two_lines, ListItem::new("c")])
            .highlight_style(Style::new().bg(Color::Blue))
            .render(area, &mut buf, &state);
        assert_eq!(
            rows_of(&buf),
            ["  a     ", "> x     ", "  y     ", "  c     "]
        );
        for y in [1, 2] {
            for x in 0..8 {
                assert_eq!(
                    buf.get(x, y).unwrap().style().bg,
                    Some(Color::Blue),
                    "({x},{y})"
                );
            }
        }
        assert_eq!(buf.get(3, 0).unwrap().style().bg, None);
        assert_eq!(buf.get(3, 3).unwrap().style().bg, None);
    }

    #[test]
    fn styles_layer_list_then_item_then_highlight() {
        let area = Rect::new(0, 0, 6, 2);
        let mut buf = Buffer::new(area);
        let mut state = ListState::new();
        state.select(Some(0));
        List::new([
            ListItem::new("a").style(Style::new().fg(Color::Green)),
            ListItem::new("b"),
        ])
        .style(Style::new().bg(Color::Black))
        .highlight_style(Style::new().bold())
        .render(area, &mut buf, &state);
        assert_eq!(
            buf.get(2, 0).unwrap().style(),
            Style::new().bg(Color::Black).fg(Color::Green).bold()
        );
        assert_eq!(
            buf.get(2, 1).unwrap().style(),
            Style::new().bg(Color::Black)
        );
    }

    #[test]
    fn a_custom_symbol_and_no_symbol() {
        let mut state = ListState::new();
        state.select(Some(0));
        assert_eq!(
            draw(List::new(["a", "b"]).highlight_symbol("→ "), &state, 5, 2),
            ["→ a  ", "  b  "]
        );
        assert_eq!(
            draw(List::new(["a", "b"]).highlight_symbol(""), &state, 3, 2),
            ["a  ", "b  "]
        );
        assert_eq!(
            draw(List::new(["a"]).highlight_symbol("中"), &state, 4, 1),
            ["中a "]
        );
    }

    #[test]
    fn selecting_below_the_view_scrolls_just_far_enough() {
        let mut state = ListState::new();
        state.select(Some(4));
        let rows = draw(List::new(items(10)), &state, 8, 3);
        assert_eq!(state.offset(), 2);
        assert_eq!(rows, ["  item 2", "  item 3", "> item 4"]);
    }

    #[test]
    fn moving_the_selection_scrolls_only_when_it_leaves_the_view() {
        let mut state = ListState::new();
        let mut offsets = Vec::new();
        for i in [0, 1, 2, 3, 4, 3, 2, 1, 0] {
            state.select(Some(i));
            draw(List::new(items(10)), &state, 8, 3);
            offsets.push(state.offset());
        }
        assert_eq!(offsets, [0, 0, 0, 1, 2, 2, 2, 1, 0]);
    }

    #[test]
    fn the_offset_persists_between_frames_and_a_selection_above_it_scrolls_up() {
        let mut state = ListState::new();
        state.select(Some(9));
        draw(List::new(items(10)), &state, 8, 3);
        assert_eq!(state.offset(), 7);
        draw(List::new(items(10)), &state, 8, 3);
        assert_eq!(state.offset(), 7);
        state.select(Some(2));
        draw(List::new(items(10)), &state, 8, 3);
        assert_eq!(state.offset(), 2);
    }

    #[test]
    fn items_of_several_rows_count_by_rows() {
        let mut state = ListState::new();
        state.select(Some(2));
        let list = List::new(["a\nb\nc", "d\ne", "f\ng", "h"]);
        let rows = draw(list, &state, 6, 4);
        // Rows: a b c | d e | f g | h. Item 2 needs rows 5-6, so item 1 leads.
        assert_eq!(state.offset(), 1);
        assert_eq!(rows, ["  d   ", "  e   ", "> f   ", "  g   "]);
    }

    #[test]
    fn the_last_item_may_be_cut_off_by_the_bottom() {
        let state = ListState::new();
        assert_eq!(
            draw(List::new(["a\nb", "c\nd"]), &state, 4, 3),
            ["  a ", "  b ", "  c "]
        );
    }

    #[test]
    fn an_item_taller_than_the_view_shows_its_top_when_selected() {
        let mut state = ListState::new();
        state.select(Some(1));
        let rows = draw(List::new(["x", "1\n2\n3\n4", "y"]), &state, 5, 2);
        assert_eq!(state.offset(), 1);
        assert_eq!(rows, ["> 1  ", "  2  "]);
    }

    #[test]
    fn shrinking_the_view_keeps_the_selection_visible() {
        let mut state = ListState::new();
        state.select(Some(5));
        draw(List::new(items(10)), &state, 8, 6);
        assert_eq!(state.offset(), 0);
        let rows = draw(List::new(items(10)), &state, 8, 2);
        assert_eq!(state.offset(), 4);
        assert_eq!(rows, ["  item 4", "> item 5"]);
    }

    #[test]
    fn a_selection_past_the_end_is_pulled_back_and_an_empty_list_is_fine() {
        let mut state = ListState::new();
        state.select(Some(50));
        let rows = draw(List::new(items(3)), &state, 8, 2);
        // It counts as the last item, which is highlighted and in view.
        assert_eq!(rows, ["  item 1", "> item 2"]);
        assert_eq!(state.offset(), 1);
        let mut state = ListState::new();
        state.select(Some(0));
        assert_eq!(
            draw(List::new(Vec::<String>::new()), &state, 4, 2),
            ["    ", "    "]
        );
        assert_eq!(state.offset(), 0);
    }

    #[test]
    fn text_is_cut_by_columns_after_the_symbol() {
        let state = ListState::new();
        assert_eq!(
            draw(List::new(["abcdef", "中文字"]), &state, 6, 2),
            ["  abcd", "  中文"]
        );
    }

    #[test]
    fn styled_spans_and_lines_in_items_keep_their_styles() {
        let line = Line::from(vec![
            Span::raw("a"),
            Span::styled("b", Style::new().fg(Color::Red)),
        ]);
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::new(area);
        List::new([ListItem::new(line)]).render(area, &mut buf, &ListState::new());
        assert_eq!(buf.get(3, 0).unwrap().style().fg, Some(Color::Red));
    }

    #[test]
    fn a_block_goes_around_the_list_and_the_items_fit_inside() {
        let mut state = ListState::new();
        state.select(Some(3));
        let list = List::new(items(6)).block(Block::bordered().title("t"));
        let rows = draw(list, &state, 10, 4);
        assert_eq!(
            rows,
            ["┌t───────┐", "│  item 2│", "│> item 3│", "└────────┘"]
        );
    }

    #[test]
    fn empty_areas_and_areas_beyond_the_buffer_are_fine() {
        let state = ListState::new();
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 3));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 4, 0),
            Rect::new(9, 9, 3, 3),
        ] {
            List::new(items(4)).render(area, &mut buf, &state);
        }
        assert!(buf.cells().iter().all(|c| c.symbol() == " "));
        // A symbol wider than the area leaves no room for text but doesn't panic.
        draw(
            List::new(items(2)).highlight_symbol("wide symbol"),
            &state,
            3,
            2,
        );
    }

    /// `App::view` only has `&self`, so a list has to be drawable from
    /// shared state, and its scroll has to survive to the next frame.
    #[test]
    fn a_list_can_be_drawn_from_a_shared_reference_and_remembers_its_scroll() {
        struct Screen {
            list: ListState,
        }
        impl Screen {
            fn view(&self, buf: &mut Buffer) {
                let area = buf.area();
                Frame::new(buf).render_stateful_widget(List::new(items(10)), area, &self.list);
            }
        }
        let mut screen = Screen {
            list: ListState::new(),
        };
        screen.list.select(Some(6));
        let mut buf = Buffer::new(Rect::new(0, 0, 8, 3));
        screen.view(&mut buf);
        assert_eq!(screen.list.offset(), 4);
        // Moving up inside the page doesn't scroll, because the offset stuck.
        screen.list.select(Some(5));
        screen.view(&mut buf);
        assert_eq!(screen.list.offset(), 4);
    }

    #[test]
    fn select_next_and_previous_stop_at_the_ends() {
        let mut s = ListState::new();
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
        let mut s = ListState::new();
        s.select_previous(4);
        assert_eq!(s.selected(), Some(3));
        s.select_next(0);
        assert_eq!(s.selected(), None);
        s.select(Some(9));
        s.select_previous(4);
        assert_eq!(s.selected(), Some(3));
    }

    #[test]
    fn frame_render_stateful_widget_clips_to_the_frame() {
        let mut buf = Buffer::new(Rect::new(0, 0, 6, 2));
        let state = ListState::new();
        Frame::new(&mut buf).render_stateful_widget(
            List::new(["a", "b"]),
            Rect::new(0, 0, 40, 40),
            &state,
        );
        assert_eq!(rows_of(&buf), ["  a   ", "  b   "]);
    }

    /// Whatever the list, view height, item heights and selection: the
    /// selected item's first row is on screen, and the offset is never
    /// beyond the selection.
    #[test]
    fn the_selected_item_is_always_in_view() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..3_000 {
            let n = next(30) as usize + 1;
            let texts: Vec<String> = (0..n)
                .map(|i| {
                    (0..=next(4))
                        .map(|k| format!("{i}.{k}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .collect();
            let h = next(8) as u16 + 1;
            let mut state = ListState::new();
            for _ in 0..8 {
                state.select(Some(next(n as u64 + 3) as usize));
                let rows = draw(List::new(texts.iter().map(String::as_str)), &state, 12, h);
                let selected = state.selected().unwrap();
                assert!(state.offset() <= selected);
                assert!(
                    rows.iter().any(|r| r.starts_with("> ")),
                    "no highlighted row: {rows:?} selected {selected} offset {}",
                    state.offset()
                );
            }
        }
    }
}

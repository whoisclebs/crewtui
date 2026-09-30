use std::cell::{Cell, RefCell};

use std::cell::Ref;

use super::paragraph::{draw_lines, line_rows};
use super::{Block, StatefulWidget, Widget, Wrap};
use crate::text::{HorizontalAlign, Line, Span, Text};
use crate::{Buffer, Rect, Style};

/// Where the view is measured: how wide and tall it was the last time it was
/// drawn, and how it wrapped.
#[derive(Debug, Clone, Copy)]
struct View {
    width: u16,
    height: u16,
    wrap: Wrap,
}

/// A row in the transcript: which entry, and how many of its rows are above.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Pos {
    entry: usize,
    row: usize,
}

/// What an entry took at some width, line by line.
#[derive(Debug, Clone)]
struct Measure {
    width: u16,
    wrap: Wrap,
    /// `ends[i]` is the rows of the lines up to and including line `i`. It
    /// may be shorter than the entry's lines, when the ones at the end have
    /// changed and have not been counted again yet.
    ends: Vec<usize>,
}

#[derive(Debug, Clone)]
struct Entry {
    text: Text<'static>,
    measure: RefCell<Option<Measure>>,
}

/// Rows before each entry, filled in as far as something asked for.
#[derive(Debug, Clone, Default)]
struct Prefix {
    key: Option<(u16, Wrap)>,
    /// `sums[i]` is the rows of all entries before entry `i`.
    sums: Vec<usize>,
}

/// How many entries one call to `content_rows` or `position` counts again
/// after a change of width, before it estimates the rest.
const COUNT_PER_CALL: usize = 160;

#[cfg(test)]
thread_local! {
    /// How many times an entry had lines counted, to check that a frame
    /// only measures what it needs.
    static MEASURED: Cell<usize> = const { Cell::new(0) };
}

/// The entries of a long transcript, and where the reader is in it.
///
/// Each entry is a [`Text`], usually one message. The state remembers how
/// many rows every entry took the last time it was measured, so drawing
/// never wraps more than what is on screen, however long the transcript is.
/// Adding to the last entry, which is what streaming does, throws away that
/// entry's count only. A change of width throws away all of them, and they
/// are counted again as they come into view.
///
/// The view either follows the end of the transcript or is anchored to a
/// row of some entry. New content moves a following view and leaves an
/// anchored one where it is, so a reader who scrolled up to read something
/// is not pulled back down by what arrives. Scrolling down to the end
/// starts following again.
///
/// ```
/// use crewtui::widgets::{History, HistoryState, StatefulWidget};
/// use crewtui::{Buffer, Rect};
///
/// let mut history = HistoryState::new();
/// history.push("you: hello");
/// history.push("agent: ");
/// history.append("hi there");
///
/// let area = Rect::new(0, 0, 20, 4);
/// let mut buf = Buffer::new(area);
/// History::new().render(area, &mut buf, &history);
/// assert!(history.is_following());
/// ```
///
/// The state is changed in `update` and only read by drawing. What drawing
/// learns, the size of the view and the rows counted, is kept in cells, so
/// [`HistoryState::scroll_up`] and the others know how far a page is.
#[derive(Debug, Clone)]
pub struct HistoryState {
    entries: Vec<Entry>,
    /// The top of the view, or `None` to follow the end.
    top: Cell<Option<Pos>>,
    view: Cell<View>,
    prefix: RefCell<Prefix>,
}

impl Default for HistoryState {
    fn default() -> Self {
        HistoryState::new()
    }
}

impl HistoryState {
    /// An empty transcript that follows its end.
    pub fn new() -> Self {
        HistoryState {
            entries: Vec::new(),
            top: Cell::new(None),
            view: Cell::new(View {
                width: 80,
                height: 24,
                wrap: Wrap::Word,
            }),
            prefix: RefCell::new(Prefix::default()),
        }
    }

    /// A state holding `entries`, following the end.
    pub fn with_entries<'a, T: Into<Text<'a>>>(entries: impl IntoIterator<Item = T>) -> Self {
        let mut state = HistoryState::new();
        for entry in entries {
            state.push(entry);
        }
        state
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when there are no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds an entry at the end. An entry with no lines takes one blank row.
    pub fn push<'a>(&mut self, text: impl Into<Text<'a>>) {
        let mut text = text.into().into_owned();
        if text.lines.is_empty() {
            text.lines.push(Line::default());
        }
        self.entries.push(Entry {
            text,
            measure: RefCell::new(None),
        });
    }

    /// The text of entry `index`.
    pub fn entry(&self, index: usize) -> Option<&Text<'static>> {
        self.entries.get(index).map(|e| &e.text)
    }

    /// The text of entry `index`, to change. Only this entry has to be
    /// measured again, all of it, since there is no telling what changed.
    /// [`HistoryState::append`] is cheaper for adding to the end.
    pub fn entry_mut(&mut self, index: usize) -> Option<&mut Text<'static>> {
        let entry = self.entries.get_mut(index)?;
        *entry.measure.get_mut() = None;
        self.prefix.get_mut().sums.truncate(index + 1);
        Some(&mut entry.text)
    }

    /// Adds `chunk` to the end of the last entry, which is how streamed
    /// tokens arrive. Text after a `\n` starts a new line of the same entry,
    /// and text keeps the style of what it follows. With no entries yet it
    /// starts one. Only the line being added to is measured again.
    pub fn append(&mut self, chunk: impl AsRef<str>) {
        let chunk = chunk.as_ref();
        if chunk.is_empty() {
            return;
        }
        if self.entries.is_empty() {
            self.push(Text::default());
        }
        let last = self.entries.len() - 1;
        self.prefix.get_mut().sums.truncate(last + 1);
        let entry = &mut self.entries[last];
        if entry.text.lines.is_empty() {
            entry.text.lines.push(Line::default());
        }
        // Everything from the current last line on is about to change.
        let keep = entry.text.lines.len() - 1;
        if let Some(measure) = entry.measure.get_mut() {
            measure.ends.truncate(keep);
        }
        let text = &mut entry.text;
        // The style of the last span there is, even when the line it is on
        // has been left behind by a newline.
        let style = text
            .lines
            .iter()
            .rev()
            .find_map(|l| l.spans.last())
            .map_or_else(Style::new, |s| s.style);
        let parts: Vec<&str> = chunk.split('\n').collect();
        for (i, part) in parts.iter().enumerate() {
            if i > 0 {
                let previous = text.lines.last().expect("an entry has a line");
                let line = Line {
                    spans: Vec::new(),
                    style: previous.style,
                    alignment: previous.alignment,
                    fill: previous.fill,
                };
                text.lines.push(line);
            }
            let line = text.lines.last_mut().expect("an entry has a line");
            if !part.is_empty() {
                match line.spans.last_mut() {
                    // Text that streams in after a link is not part of it.
                    Some(span) if span.link.is_none() => span.content.to_mut().push_str(part),
                    Some(span) => {
                        let style = span.style;
                        line.spans.push(Span::styled((*part).to_owned(), style));
                    }
                    None => line.spans.push(Span::styled((*part).to_owned(), style)),
                }
            }
            // A `\r` that ended one chunk is dropped when the `\n` after it
            // arrives, whichever chunk that is in.
            if i + 1 < parts.len() {
                if let Some(span) = line.spans.last_mut() {
                    if span.content.ends_with('\r') {
                        span.content.to_mut().pop();
                    }
                }
            }
        }
    }

    /// Removes everything and goes back to following the end.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.top.set(None);
        *self.prefix.get_mut() = Prefix::default();
    }

    /// Whether the view follows the end of the transcript.
    pub fn is_following(&self) -> bool {
        self.top.get().is_none()
    }

    /// Rows the whole transcript takes at the width it was last drawn at.
    ///
    /// After a change of width the entries are counted again, 160 per call,
    /// so that a scrollbar doesn't stall a frame on a very long transcript.
    /// Until every entry is counted the result is an estimate: the rows
    /// counted so far plus their average for each entry left. It settles as
    /// this is called again, which a scrollbar does on every frame. It is
    /// exact once everything is counted, and at once for a transcript of up
    /// to 160 entries.
    pub fn content_rows(&self) -> usize {
        self.count_more(self.entries.len());
        self.rows_before(self.entries.len())
    }

    /// The row at the top of the view, counted from the start of the
    /// transcript. Together with [`HistoryState::content_rows`] and
    /// [`HistoryState::viewport_rows`] this is what a
    /// [`Scrollbar`](crate::widgets::Scrollbar) needs.
    ///
    /// This is 0 at the start of the transcript, and when following the end it
    /// is where the scrollbar thumb sits at the bottom of `content_rows`, as
    /// long as both are asked for in the same frame. Anywhere else it is an
    /// estimate, like `content_rows`, until everything is counted, and it
    /// never goes past the last position the scrollbar can show.
    pub fn position(&self) -> usize {
        if self.entries.is_empty() {
            return 0;
        }
        self.count_first_batch();
        let end = self
            .rows_before(self.entries.len())
            .saturating_sub(self.viewport_rows());
        if self.is_following() {
            return end;
        }
        let top = self.resolve_top();
        (self.rows_before(top.entry) + top.row).min(end)
    }

    /// How many rows the view had the last time it was drawn.
    pub fn viewport_rows(&self) -> usize {
        usize::from(self.view.get().height)
    }

    /// Moves the view up by `rows`, which stops following the end.
    pub fn scroll_up(&mut self, rows: usize) {
        if rows == 0 || self.entries.is_empty() {
            return;
        }
        let next = self.step_up(self.resolve_top(), rows);
        self.set_top(next);
    }

    /// Moves the view down by `rows`. Reaching the end starts following it.
    pub fn scroll_down(&mut self, rows: usize) {
        if rows == 0 || self.top.get().is_none() {
            return;
        }
        let next = self.step_down(self.resolve_top(), rows);
        self.set_top(next);
    }

    /// Moves up by one screen, keeping one row of overlap.
    pub fn page_up(&mut self) {
        self.scroll_up(self.page());
    }

    /// Moves down by one screen, keeping one row of overlap.
    pub fn page_down(&mut self) {
        self.scroll_down(self.page());
    }

    /// Goes to the start of the transcript.
    pub fn scroll_to_top(&mut self) {
        if !self.entries.is_empty() {
            self.set_top(Pos { entry: 0, row: 0 });
        }
    }

    /// Goes to the end and follows it from now on.
    pub fn scroll_to_bottom(&mut self) {
        self.top.set(None);
    }

    fn page(&self) -> usize {
        self.viewport_rows().saturating_sub(1).max(1)
    }

    /// Anchors the view at `pos`, unless that is where following would be.
    fn set_top(&mut self, pos: Pos) {
        let top = if pos >= self.tail_top() {
            None
        } else {
            Some(pos)
        };
        self.top.set(top);
    }

    /// The lines of entry `i` counted at the current view, counting the
    /// ones that are not yet.
    fn measured(&self, i: usize) -> Ref<'_, Measure> {
        let view = self.view.get();
        let entry = &self.entries[i];
        {
            let mut slot = entry.measure.borrow_mut();
            if slot
                .as_ref()
                .is_none_or(|m| m.width != view.width || m.wrap != view.wrap)
            {
                *slot = Some(Measure {
                    width: view.width,
                    wrap: view.wrap,
                    ends: Vec::new(),
                });
            }
            let measure = slot.as_mut().expect("it was just set");
            if measure.ends.len() < entry.text.lines.len() {
                #[cfg(test)]
                MEASURED.with(|c| c.set(c.get() + 1));
                let mut total = measure.ends.last().copied().unwrap_or(0);
                for line in &entry.text.lines[measure.ends.len()..] {
                    total += line_rows(line, usize::from(view.width), view.wrap);
                    measure.ends.push(total);
                }
            }
        }
        Ref::map(entry.measure.borrow(), |m| {
            m.as_ref().expect("it was just measured")
        })
    }

    /// Rows of entry `i` at the current view. Never less than one.
    fn rows_of(&self, i: usize) -> usize {
        self.measured(i).ends.last().copied().unwrap_or(0).max(1)
    }

    /// Which line of entry `i` its row `row` is on, and how many rows of that
    /// line are above it.
    fn line_at(&self, i: usize, row: usize) -> (usize, usize) {
        let measure = self.measured(i);
        let line = measure.ends.partition_point(|&end| end <= row);
        let above = if line == 0 { 0 } else { measure.ends[line - 1] };
        (line, row - above)
    }

    /// Counts the entries that were not counted yet at the current width, in
    /// order, up to entry `upto` and at most `COUNT_PER_CALL` of them.
    fn count_more(&self, upto: usize) {
        let view = self.view.get();
        let mut prefix = self.prefix.borrow_mut();
        if prefix.key != Some((view.width, view.wrap)) {
            prefix.key = Some((view.width, view.wrap));
            prefix.sums.clear();
        }
        if prefix.sums.is_empty() {
            prefix.sums.push(0);
        }
        let stop = upto.min(prefix.sums.len() - 1 + COUNT_PER_CALL);
        while prefix.sums.len() <= stop {
            let j = prefix.sums.len() - 1;
            let next = prefix.sums[j] + self.rows_of(j);
            prefix.sums.push(next);
        }
    }

    /// Rows of all entries before entry `i`, from what is counted so far: exact
    /// when entry `i` is reached, and otherwise the counted rows plus the
    /// average of them for each entry not counted yet.
    fn rows_before(&self, i: usize) -> usize {
        let prefix = self.prefix.borrow();
        let counted = prefix.sums.len().saturating_sub(1);
        if i <= counted {
            return prefix.sums.get(i).copied().unwrap_or(0);
        }
        if counted == 0 {
            // Nothing counted, which only happens when nothing was asked for
            // yet: a row per entry is the least an entry takes.
            return i;
        }
        let so_far = prefix.sums[counted];
        so_far + (i - counted) * so_far / counted
    }

    /// Counts one batch, unless a batch has been counted already. That makes
    /// a transcript of up to `COUNT_PER_CALL` entries exact for whoever asks,
    /// and leaves the counting of a longer one to `content_rows`, so that the
    /// two agree within a frame however they are called.
    fn count_first_batch(&self) {
        let counted = self.prefix.borrow().sums.len().saturating_sub(1);
        if counted < COUNT_PER_CALL.min(self.entries.len()) {
            self.count_more(self.entries.len());
        }
    }

    /// Where the top of the view is when it shows the end.
    fn tail_top(&self) -> Pos {
        let height = self.viewport_rows();
        let mut rows = 0;
        for i in (0..self.entries.len()).rev() {
            rows += self.rows_of(i);
            if rows >= height {
                return Pos {
                    entry: i,
                    row: rows - height,
                };
            }
        }
        Pos { entry: 0, row: 0 }
    }

    /// Where the top of the view is right now: the anchor, held inside the
    /// transcript as it is at this width, or the end when following. An
    /// anchor past what the view can show is pulled back to the end, and
    /// stays there, so a transcript that shrank never leaves blank rows below
    /// and content that arrives later does not carry the reader along.
    fn resolve_top(&self) -> Pos {
        let tail = self.tail_top();
        let Some(anchor) = self.top.get() else {
            return tail;
        };
        let pos = if anchor.entry >= self.entries.len() {
            tail
        } else {
            let row = anchor.row.min(self.rows_of(anchor.entry) - 1);
            Pos {
                entry: anchor.entry,
                row,
            }
            .min(tail)
        };
        self.top.set(Some(pos));
        pos
    }

    /// `pos` moved `n` rows toward the start. Stops at the first row.
    fn step_up(&self, mut pos: Pos, mut n: usize) -> Pos {
        loop {
            if n <= pos.row {
                pos.row -= n;
                return pos;
            }
            n -= pos.row;
            if pos.entry == 0 {
                return Pos { entry: 0, row: 0 };
            }
            pos.entry -= 1;
            pos.row = self.rows_of(pos.entry);
        }
    }

    /// `pos` moved `n` rows toward the end. Past the last row it is
    /// `(len, 0)`.
    fn step_down(&self, mut pos: Pos, mut n: usize) -> Pos {
        loop {
            if pos.entry >= self.entries.len() {
                return Pos {
                    entry: self.entries.len(),
                    row: 0,
                };
            }
            let rows = self.rows_of(pos.entry);
            let left = rows.saturating_sub(pos.row);
            if n < left {
                pos.row += n;
                return pos;
            }
            n -= left;
            pos = Pos {
                entry: pos.entry + 1,
                row: 0,
            };
        }
    }
}

/// Draws a [`HistoryState`]: the part of the transcript the view is on.
///
/// Entries follow each other with no gap, wrapped at the width of the area.
/// Only the entries that are on screen are wrapped for drawing, and only
/// the ones from the end of the transcript to the top of the view are
/// measured, so a frame costs what fits on screen and not what has been
/// said. See [`HistoryState`] for how the view moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct History<'a> {
    block: Option<Block<'a>>,
    style: Style,
    wrap: Wrap,
    align: HorizontalAlign,
}

impl<'a> History<'a> {
    /// A history wrapped by word, left aligned, with no block.
    pub fn new() -> Self {
        History {
            block: None,
            style: Style::new(),
            wrap: Wrap::Word,
            align: HorizontalAlign::Left,
        }
    }

    /// A block drawn around the history.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The style under every entry.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// How long lines are wrapped. Word wrapping by default.
    pub fn wrap(mut self, wrap: Wrap) -> Self {
        self.wrap = wrap;
        self
    }

    /// How lines are aligned.
    pub fn align(mut self, align: HorizontalAlign) -> Self {
        self.align = align;
        self
    }
}

impl Default for History<'_> {
    fn default() -> Self {
        History::new()
    }
}

impl StatefulWidget for History<'_> {
    type State = HistoryState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &HistoryState) {
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
        state.view.set(View {
            width: area.width,
            height: area.height,
            wrap: self.wrap,
        });
        if state.entries.is_empty() {
            return;
        }
        let height = usize::from(area.height);
        let mut pos = state.resolve_top();
        let mut used = 0;
        while pos.entry < state.entries.len() && used < height {
            let rest = Rect {
                y: area.y + used as u16,
                height: (height - used) as u16,
                ..area
            };
            let text = &state.entries[pos.entry].text;
            // Start at the line the row falls on, not at the first one.
            let (line, skip) = state.line_at(pos.entry, pos.row);
            // An entry someone emptied out through `entry_mut` still takes
            // the one blank row it is counted as.
            used += draw_lines(
                buf,
                rest,
                &text.lines[line.min(text.lines.len())..],
                self.style.patch(text.style),
                self.wrap,
                self.align,
                skip,
            )
            .max(1);
            pos = Pos {
                entry: pos.entry + 1,
                row: 0,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::Paragraph;
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

    fn draw_with(history: History<'_>, state: &HistoryState, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(area);
        history.render(area, &mut buf, state);
        rows_of(&buf)
    }

    fn draw(state: &HistoryState, w: u16, h: u16) -> Vec<String> {
        draw_with(History::new(), state, w, h)
    }

    #[test]
    fn a_full_width_entry_tints_its_rows_to_the_edge_and_nothing_else() {
        let mut s = HistoryState::new();
        s.push(
            Text::raw("aaaa bbbb")
                .full_width()
                .style(Style::new().bg(Color::Blue)),
        );
        s.push("plain");
        let area = Rect::new(0, 0, 8, 4);
        let mut buf = Buffer::new(area);
        History::new().wrap(Wrap::Word).render(area, &mut buf, &s);
        let bg = |x, y| buf.get(x, y).unwrap().style().bg;
        // "aaaa bbbb" wraps to two rows, and both are tinted end to end.
        for y in 0..2 {
            for x in 0..8 {
                assert_eq!(bg(x, y), Some(Color::Blue), "({x},{y})");
            }
        }
        assert_eq!(bg(7, 2), None);
        assert_eq!(bg(0, 2), None);
    }

    #[test]
    fn a_streamed_line_of_a_full_width_entry_stays_full_width() {
        let mut s = HistoryState::new();
        s.push(
            Text::raw("a")
                .full_width()
                .style(Style::new().bg(Color::Red)),
        );
        s.append("b\nc");
        let entry = s.entry(0).unwrap();
        assert_eq!(entry.lines.len(), 2);
        assert!(entry.lines.iter().all(|l| l.fill));
        let area = Rect::new(0, 0, 5, 2);
        let mut buf = Buffer::new(area);
        History::new().render(area, &mut buf, &s);
        assert_eq!(buf.get(4, 1).unwrap().style().bg, Some(Color::Red));
    }

    fn numbered(n: usize) -> HistoryState {
        let mut s = HistoryState::new();
        for i in 0..n {
            s.push(format!("line {i}"));
        }
        s
    }

    fn measured<R>(f: impl FnOnce() -> R) -> (R, usize) {
        MEASURED.with(|c| c.set(0));
        let r = f();
        (r, MEASURED.with(Cell::get))
    }

    #[test]
    fn a_short_transcript_starts_at_the_top() {
        let s = numbered(2);
        assert_eq!(
            draw(&s, 8, 4),
            ["line 0  ", "line 1  ", "        ", "        "]
        );
        assert!(s.is_following());
    }

    #[test]
    fn a_long_transcript_shows_its_end_and_follows_new_entries() {
        let mut s = numbered(10);
        assert_eq!(draw(&s, 8, 3), ["line 7  ", "line 8  ", "line 9  "]);
        s.push("line 10");
        assert_eq!(draw(&s, 8, 3), ["line 8  ", "line 9  ", "line 10 "]);
    }

    #[test]
    fn entries_wrap_and_the_view_counts_wrapped_rows() {
        let mut s = HistoryState::new();
        s.push("aaa bbb ccc");
        s.push("dd");
        assert_eq!(draw(&s, 4, 3), ["bbb ", "ccc ", "dd  "]);
        assert_eq!(s.content_rows(), 4);
    }

    #[test]
    fn scrolling_up_stops_following_and_new_content_does_not_drag_the_view() {
        let mut s = numbered(10);
        draw(&s, 8, 3);
        s.scroll_up(3);
        assert!(!s.is_following());
        assert_eq!(draw(&s, 8, 3), ["line 4  ", "line 5  ", "line 6  "]);
        for i in 0..50 {
            s.push(format!("new {i}"));
            s.append(" and more\nlines");
        }
        assert_eq!(draw(&s, 8, 3), ["line 4  ", "line 5  ", "line 6  "]);
        assert_eq!(s.position(), 4);
        assert!(!s.is_following());
    }

    #[test]
    fn growing_the_last_entry_does_not_move_a_reader_inside_it() {
        let mut s = HistoryState::new();
        s.push("head");
        s.push("t1\nt2\nt3\nt4");
        draw(&s, 8, 2);
        s.scroll_up(1);
        assert_eq!(draw(&s, 8, 2), ["t2      ", "t3      "]);
        s.append("\nt5\nt6");
        assert_eq!(draw(&s, 8, 2), ["t2      ", "t3      "]);
    }

    #[test]
    fn scrolling_down_to_the_end_follows_again() {
        let mut s = numbered(10);
        draw(&s, 8, 3);
        s.scroll_up(5);
        s.push("line 10");
        s.scroll_down(1);
        assert!(!s.is_following());
        // The view is on 3..6; the end is at 8, so five more rows get there.
        s.scroll_down(4);
        assert!(!s.is_following());
        s.scroll_down(1);
        assert!(s.is_following());
        assert_eq!(draw(&s, 8, 3), ["line 8  ", "line 9  ", "line 10 "]);
        s.push("line 11");
        assert_eq!(draw(&s, 8, 3), ["line 9  ", "line 10 ", "line 11 "]);
    }

    #[test]
    fn scrolling_past_either_end_stops_at_it() {
        let mut s = numbered(10);
        draw(&s, 8, 3);
        s.scroll_up(1000);
        assert_eq!(draw(&s, 8, 3), ["line 0  ", "line 1  ", "line 2  "]);
        assert_eq!(s.position(), 0);
        s.scroll_up(1);
        assert_eq!(s.position(), 0);
        s.scroll_down(1000);
        assert!(s.is_following());
        assert_eq!(s.position(), 7);
        s.scroll_down(1);
        assert_eq!(s.position(), 7);
    }

    #[test]
    fn scroll_by_zero_and_on_nothing_change_nothing() {
        let mut s = numbered(10);
        draw(&s, 8, 3);
        s.scroll_up(0);
        assert!(s.is_following());
        s.scroll_down(5);
        assert!(s.is_following());
        let mut empty = HistoryState::new();
        empty.scroll_up(3);
        empty.scroll_to_top();
        empty.page_up();
        assert!(empty.is_following());
        assert_eq!(empty.position(), 0);
        assert_eq!(empty.content_rows(), 0);
        assert_eq!(draw(&empty, 4, 2), ["    ", "    "]);
    }

    #[test]
    fn content_that_fits_stays_following_when_scrolled() {
        let mut s = numbered(2);
        draw(&s, 8, 4);
        s.scroll_up(3);
        s.scroll_to_top();
        assert!(s.is_following());
        s.push("line 2");
        assert_eq!(draw(&s, 8, 4)[2], "line 2  ");
    }

    #[test]
    fn pages_move_by_the_view_height_less_one_row() {
        let mut s = numbered(30);
        draw(&s, 8, 5);
        s.page_up();
        assert_eq!(draw(&s, 8, 5)[0], "line 21 ");
        s.page_up();
        assert_eq!(draw(&s, 8, 5)[0], "line 17 ");
        s.page_down();
        s.page_down();
        assert!(s.is_following());
        assert_eq!(draw(&s, 8, 5)[0], "line 25 ");
    }

    #[test]
    fn to_top_and_to_bottom() {
        let mut s = numbered(30);
        draw(&s, 8, 5);
        s.scroll_to_top();
        assert!(!s.is_following());
        assert_eq!(draw(&s, 8, 5)[0], "line 0  ");
        s.scroll_to_bottom();
        assert!(s.is_following());
        assert_eq!(draw(&s, 8, 5)[4], "line 29 ");
    }

    fn wide_entries(n: usize) -> HistoryState {
        let mut s = HistoryState::new();
        for i in 0..n {
            s.push(format!("{i}: aaaa bbbb cccc dddd"));
        }
        s
    }

    #[test]
    fn a_resize_keeps_the_reader_on_the_same_entry() {
        let mut s = wide_entries(12);
        draw(&s, 30, 3);
        s.scroll_up(6);
        assert!(draw(&s, 30, 3)[0].starts_with("3:"));
        // Four rows per entry at this width.
        assert!(draw(&s, 8, 3)[0].starts_with("3: aaaa"));
        assert!(draw(&s, 4, 3)[0].starts_with("3:"));
        assert!(draw(&s, 30, 3)[0].starts_with("3:"));
    }

    #[test]
    fn a_row_that_no_longer_exists_after_a_resize_is_held_inside_its_entry() {
        let s = wide_entries(12);
        draw(&s, 8, 3);
        s.top.set(Some(Pos { entry: 5, row: 2 }));
        assert_eq!(draw(&s, 8, 3), ["cccc    ", "dddd    ", "6: aaaa "]);
        assert!(draw(&s, 30, 3)[0].starts_with("5: aaaa"));
    }

    #[test]
    fn a_resize_while_following_keeps_following() {
        let s = wide_entries(12);
        assert!(draw(&s, 30, 2)[1].starts_with("11:"));
        assert!(draw(&s, 8, 2)[1].starts_with("dddd"));
        assert!(s.is_following());
    }

    #[test]
    fn a_transcript_that_shrinks_under_the_reader_shows_a_full_view() {
        let mut s = HistoryState::new();
        s.push("a");
        s.push("b");
        s.push(Text::raw("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\nl8\nl9"));
        draw(&s, 8, 3);
        s.top.set(Some(Pos { entry: 2, row: 7 }));
        assert_eq!(draw(&s, 8, 3), ["l7      ", "l8      ", "l9      "]);
        *s.entry_mut(2).unwrap() = Text::raw("l0\nl1");
        assert_eq!(draw(&s, 8, 3), ["b       ", "l0      ", "l1      "]);
        assert!(!s.is_following());
        s.clear();
        s.push("only");
        assert!(s.is_following());
        assert_eq!(draw(&s, 8, 3)[0], "only    ");
    }

    #[test]
    fn append_extends_the_last_line_and_splits_at_newlines() {
        let mut s = HistoryState::new();
        s.append("hel");
        s.append("lo");
        assert_eq!(s.len(), 1);
        s.append(" there\r\nsecond");
        s.append("");
        s.append("\n\nfourth");
        let lines: Vec<String> = s
            .entry(0)
            .unwrap()
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(lines, ["hello there", "second", "", "fourth"]);
    }

    #[test]
    fn streamed_tokens_stay_in_one_span_and_keep_the_style() {
        let red = Style::new().fg(Color::Red);
        let mut s = HistoryState::new();
        s.push(Line::from(Span::styled("agent: ", red)));
        for _ in 0..100 {
            s.append("tok ");
        }
        let text = s.entry(0).unwrap();
        assert_eq!(text.lines[0].spans.len(), 1);
        assert_eq!(text.lines[0].spans[0].style, red);
        s.append("\nnext");
        let second = &s.entry(0).unwrap().lines[1];
        assert_eq!(second.spans[0].style, red);
    }

    #[test]
    fn a_following_view_shows_the_streamed_end() {
        let mut s = HistoryState::new();
        s.push("agent:");
        assert_eq!(draw(&s, 8, 3)[0], "agent:  ");
        s.append("\nl2\nl3\nl4");
        assert_eq!(draw(&s, 8, 3), ["l2      ", "l3      ", "l4      "]);
    }

    #[test]
    fn an_entry_with_no_lines_takes_one_blank_row() {
        let mut s = HistoryState::new();
        s.push("a");
        s.push(Text::default());
        s.push("");
        s.push("b");
        assert_eq!(s.content_rows(), 4);
        assert_eq!(draw(&s, 4, 4), ["a   ", "    ", "    ", "b   "]);
    }

    #[test]
    fn changing_an_entry_measures_it_again() {
        let mut s = wide_entries(3);
        assert_eq!(draw(&s, 30, 3)[2].trim_end(), "2: aaaa bbbb cccc dddd");
        assert_eq!(s.content_rows(), 3);
        *s.entry_mut(1).unwrap() = Text::raw("x\ny\nz");
        assert_eq!(s.content_rows(), 5);
        assert!(s.entry_mut(9).is_none());
    }

    #[test]
    fn a_frame_measures_only_what_it_shows() {
        let mut s = HistoryState::new();
        for i in 0..20_000 {
            s.push(format!("entry {i}"));
        }
        let (_, n) = measured(|| draw(&s, 40, 20));
        assert!(n <= 20, "the first frame measured {n} entries");
        let (_, n) = measured(|| draw(&s, 40, 20));
        assert_eq!(n, 0, "a repeated frame measured {n} entries");

        s.push("agent:");
        for _ in 0..100 {
            s.append("tok ");
            let (_, n) = measured(|| draw(&s, 40, 20));
            assert!(n <= 2, "a streamed token measured {n} entries");
        }

        s.scroll_up(100);
        let (_, n) = measured(|| draw(&s, 40, 20));
        assert_eq!(n, 0, "the frame after scrolling measured {n} entries");

        // A new width makes every count stale, but only what is shown is
        // counted again.
        // Counted twice at most: the entries the view is anchored at, and the
        // ones at the end that say where the view can go.
        let (_, n) = measured(|| draw(&s, 30, 20));
        assert!(n <= 45, "a resize measured {n} entries");
        s.scroll_to_bottom();
        let (_, n) = measured(|| draw(&s, 25, 20));
        assert!(n <= 25, "a resize while following measured {n} entries");
    }

    #[test]
    fn text_streamed_in_after_a_link_is_not_part_of_it() {
        let mut s = HistoryState::new();
        s.push(Line::from(vec![
            Span::raw("see "),
            Span::raw("docs").link("https://e.com"),
        ]));
        s.append(" then more\nnext");
        let entry = s.entry(0).unwrap();
        let first = &entry.lines[0].spans;
        assert_eq!(first[1].content, "docs");
        assert_eq!(first[1].link.as_deref(), Some("https://e.com"));
        assert_eq!(first[2].content, " then more");
        assert_eq!(first[2].link, None);
        // A link on the last span of a line keeps its style for what follows.
        s.append(" and on");
        let last = s.entry(0).unwrap().lines[1].spans.last().unwrap();
        assert_eq!(last.link, None);
    }

    #[test]
    fn a_state_can_be_built_from_entries_and_takes_owned_or_borrowed_chunks() {
        let mut s = HistoryState::with_entries(["one", "two"]);
        assert_eq!(s.len(), 2);
        assert!(s.is_following());
        s.append("!");
        s.append(String::from("?"));
        let tail = String::from(".");
        s.append(&tail);
        let _ = &tail;
        let text: String = s.entry(1).unwrap().lines[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(text, "two!?.");
    }

    #[test]
    fn counting_the_whole_transcript_is_spread_over_calls() {
        let mut s = HistoryState::new();
        for i in 0..2_000 {
            s.push(format!("entry {i}"));
        }
        draw(&s, 40, 20);
        // Each call counts a batch and estimates the rest from it.
        let (rows, n) = measured(|| s.content_rows());
        assert_eq!(
            rows, 2_000,
            "every entry is one row, so the estimate is exact"
        );
        assert!(n <= COUNT_PER_CALL, "a call counted {n} entries");
        for _ in 0..(2_000 / COUNT_PER_CALL) {
            s.content_rows();
        }
        let (rows, n) = measured(|| s.content_rows());
        assert_eq!((rows, n), (2_000, 0));
        s.append("more");
        let (_, n) = measured(|| s.content_rows());
        assert!(n <= 1, "{n}");
    }

    /// A transcript whose first entries are nothing like the rest gives a bad
    /// first estimate. The scrollbar still has to be consistent in every
    /// frame: the thumb at the bottom while following, and never a position
    /// past the end.
    #[test]
    fn the_thumb_stays_consistent_while_the_estimate_settles() {
        let long = "aaaa bbbb cccc dddd ".repeat(5);
        let mut s = HistoryState::new();
        for i in 0..20_000 {
            s.push(if i < COUNT_PER_CALL {
                long.as_str()
            } else {
                "a"
            });
        }
        draw(&s, 120, 10);
        for width in [30, 25] {
            draw(&s, width, 10);
            for frame in 0..200 {
                let content = s.content_rows();
                let position = s.position();
                assert_eq!(
                    position,
                    content.saturating_sub(s.viewport_rows()),
                    "following, width {width}, frame {frame}"
                );
            }
        }
        // Anchored: never past the end, whatever the estimate says.
        s.scroll_up(10_000);
        for width in [30, 20] {
            draw(&s, width, 10);
            for frame in 0..200 {
                let content = s.content_rows();
                let position = s.position();
                assert!(
                    position <= content.saturating_sub(s.viewport_rows()),
                    "anchored, width {width}, frame {frame}: {position} of {content}"
                );
            }
        }
    }

    #[test]
    fn the_estimate_settles_on_the_exact_count_and_the_ends_are_exact_before() {
        let mut s = HistoryState::new();
        for i in 0..3_000 {
            // One row, or two at width 10.
            s.push(if i % 4 == 0 {
                "aaaa bbbb cccc dddd"
            } else {
                "aa"
            });
        }
        draw(&s, 10, 5);
        let exact = 750 * 2 + 2_250;
        // Following: the thumb is at the very end, whatever the estimate is.
        let first = s.content_rows();
        assert!(first.abs_diff(exact) < exact / 10, "{first} vs {exact}");
        assert_eq!(s.position(), first - s.viewport_rows());
        // At the start: exact at once.
        s.scroll_to_top();
        assert_eq!(s.position(), 0);
        for _ in 0..(3_000 / COUNT_PER_CALL + 1) {
            s.content_rows();
        }
        assert_eq!(s.content_rows(), exact);
        s.scroll_to_bottom();
        assert_eq!(s.position(), exact - 5);
    }

    /// The view shows exactly what a paragraph of all the entries scrolled
    /// to the same row would show, and position and content agree with it.
    #[test]
    fn the_view_matches_a_paragraph_of_the_whole_transcript() {
        let words = [
            "a",
            "bb",
            "ccc",
            "dddddddddd",
            "中文",
            "😀",
            "e\u{301}x",
            " ",
            "  ",
            "\n",
            "x y z",
            "\n\n",
            "longer words here",
        ];
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut next = move |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        for round in 0..300 {
            let wrap = [Wrap::Word, Wrap::Char, Wrap::None][round % 3];
            let mut s = HistoryState::new();
            let mut all = Text::default();
            for _ in 0..next(12) + 1 {
                let mut src = String::new();
                for _ in 0..next(8) + 1 {
                    src.push_str(words[next(words.len())]);
                }
                let text = Text::raw(src);
                all.lines.extend(text.lines.clone());
                s.push(text);
            }
            let w = (next(12) + 1) as u16;
            let h = (next(8) + 1) as u16;
            let total: usize = all
                .lines
                .iter()
                .map(|l| line_rows(l, usize::from(w), wrap))
                .sum();
            let render = |s: &HistoryState| draw_with(History::new().wrap(wrap), s, w, h);
            let expect = |position: usize| {
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::new(area);
                Paragraph::new(all.clone())
                    .wrap(wrap)
                    .scroll(position)
                    .render(area, &mut buf);
                rows_of(&buf)
            };
            let max = total.saturating_sub(usize::from(h));
            assert_eq!(render(&s), expect(max), "round {round}");
            assert_eq!(s.content_rows(), total, "round {round}");
            assert_eq!(s.position(), max, "round {round}");
            for step in 0..12 {
                match next(6) {
                    0 => s.scroll_up(next(6) + 1),
                    1 => s.scroll_down(next(6) + 1),
                    2 => s.page_up(),
                    3 => s.page_down(),
                    4 => s.scroll_to_top(),
                    _ => s.scroll_to_bottom(),
                }
                let shown = render(&s);
                let at = s.position();
                assert!(at <= max, "round {round} step {step}");
                assert_eq!(shown, expect(at), "round {round} step {step}");
                assert_eq!(s.is_following(), at == max, "round {round} step {step}");
            }
        }
    }

    #[test]
    fn wrap_none_cuts_lines_and_align_places_them() {
        let mut s = HistoryState::new();
        s.push("abcdefghij");
        s.push("ab");
        assert_eq!(
            draw_with(History::new().wrap(Wrap::None), &s, 4, 2),
            ["abcd", "ab  "]
        );
        assert_eq!(
            draw_with(History::new().align(HorizontalAlign::Right), &s, 12, 3)[1],
            "          ab"
        );
    }

    #[test]
    fn a_block_goes_around_and_the_view_is_measured_inside_it() {
        let s = numbered(10);
        let area = Rect::new(0, 0, 10, 5);
        let mut buf = Buffer::new(area);
        History::new()
            .block(Block::bordered())
            .render(area, &mut buf, &s);
        assert_eq!(
            rows_of(&buf),
            [
                "┌────────┐",
                "│line 7  │",
                "│line 8  │",
                "│line 9  │",
                "└────────┘"
            ]
        );
        assert_eq!(s.viewport_rows(), 3);
    }

    #[test]
    fn the_style_goes_under_the_text_and_the_entry_style_over_it() {
        let mut s = HistoryState::new();
        s.push(Text::raw("hi").style(Style::new().fg(Color::Red)));
        let area = Rect::new(0, 0, 4, 2);
        let mut buf = Buffer::new(area);
        History::new()
            .style(Style::new().bg(Color::Blue))
            .render(area, &mut buf, &s);
        let cell = buf.get(0, 0).unwrap().style();
        assert_eq!((cell.fg, cell.bg), (Some(Color::Red), Some(Color::Blue)));
        assert_eq!(buf.get(3, 1).unwrap().style().bg, Some(Color::Blue));
    }

    #[test]
    fn empty_areas_and_areas_beyond_the_buffer_are_fine() {
        let s = numbered(5);
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 3));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 4, 0),
            Rect::new(9, 9, 3, 3),
        ] {
            History::new().render(area, &mut buf, &s);
        }
        History::new()
            .block(Block::bordered())
            .render(Rect::new(0, 0, 2, 2), &mut buf, &s);
        // Only the border of the block that had no room inside got drawn.
        assert!(buf.cells().iter().all(|c| !c.symbol().starts_with('l')));
        assert_eq!(s.viewport_rows(), 24, "nothing was drawn, nothing measured");
    }

    #[test]
    fn a_frame_draws_it_as_a_stateful_widget() {
        let s = numbered(5);
        let area = Rect::new(0, 0, 8, 2);
        let mut buf = Buffer::new(area);
        let mut frame = Frame::new(&mut buf);
        frame.render_stateful_widget(History::new(), area, &s);
        assert_eq!(rows_of(&buf), ["line 3  ", "line 4  "]);
    }

    #[test]
    fn it_works_with_a_scrollbar() {
        use crate::widgets::Scrollbar;
        let mut s = numbered(100);
        draw(&s, 8, 10);
        let bar = |s: &HistoryState| {
            let area = Rect::new(0, 0, 1, 10);
            let mut buf = Buffer::new(area);
            Scrollbar::vertical()
                .content(s.content_rows())
                .viewport(s.viewport_rows())
                .position(s.position())
                .render(area, &mut buf);
            rows_of(&buf).concat()
        };
        assert_eq!(bar(&s), "│││││││││█");
        s.scroll_to_top();
        assert_eq!(bar(&s), "█│││││││││");
    }

    #[test]
    fn owned_and_borrowed_text_can_be_pushed() {
        let local = String::from("borrowed");
        let mut s = HistoryState::new();
        s.push(local.as_str());
        s.push(Line::raw(local.as_str()));
        s.push(Span::raw(local.as_str()));
        s.push(local.clone());
        drop(local);
        assert_eq!(s.len(), 4);
        assert_eq!(draw(&s, 10, 4)[3], "borrowed  ");
    }

    #[test]
    fn an_entry_emptied_out_still_takes_its_blank_row() {
        let mut s = HistoryState::new();
        s.push("a");
        s.push("b");
        *s.entry_mut(0).unwrap() = Text::default();
        assert_eq!(s.content_rows(), 2);
        assert_eq!(draw(&s, 4, 2), ["    ", "b   "]);
    }

    #[test]
    fn the_row_counts_follow_the_width() {
        let mut s = HistoryState::new();
        s.push("aaa bbb");
        s.push("cc");
        draw(&s, 8, 3);
        assert_eq!(s.content_rows(), 2);
        draw(&s, 4, 3);
        assert_eq!(s.content_rows(), 3);
        draw_with(History::new().wrap(Wrap::None), &s, 4, 3);
        assert_eq!(s.content_rows(), 2);
    }

    #[test]
    fn appending_to_an_entry_that_was_emptied_out_starts_its_first_line() {
        let mut s = HistoryState::new();
        s.push("a");
        *s.entry_mut(0).unwrap() = Text::default();
        s.append("x");
        assert_eq!(draw(&s, 4, 1), ["x   "]);
        *s.entry_mut(0).unwrap() = Text::default();
        s.append("\ny");
        assert_eq!(draw(&s, 4, 2), ["    ", "y   "]);
    }

    #[test]
    fn scrolling_down_by_an_enormous_amount_goes_to_the_end() {
        let mut s = HistoryState::new();
        for i in 0..8 {
            s.push(format!("{i}a\n{i}b\n{i}c"));
        }
        draw(&s, 8, 4);
        s.scroll_up(13);
        assert!(!s.is_following());
        s.scroll_down(usize::MAX);
        assert!(s.is_following());
        s.scroll_up(1);
        s.scroll_down(usize::MAX - 1);
        assert!(s.is_following());
        assert_eq!(draw(&s, 8, 4)[3], "7c      ");
    }

    #[test]
    fn streamed_text_keeps_the_style_when_the_newline_comes_first() {
        let red = Style::new().fg(Color::Red);
        let mut s = HistoryState::new();
        s.push(Line::from(Span::styled("agent: ", red)));
        s.append("tok\n");
        s.append("next");
        let text = s.entry(0).unwrap();
        assert_eq!(text.lines.len(), 2);
        assert_eq!(text.lines[1].spans[0].content, "next");
        assert_eq!(text.lines[1].spans[0].style, red);
    }

    #[test]
    fn a_carriage_return_before_a_newline_is_dropped_across_chunks() {
        let mut s = HistoryState::new();
        s.append("a\r");
        s.append("\nb\r");
        s.append("\r\nc");
        let lines: Vec<String> = s
            .entry(0)
            .unwrap()
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(lines, ["a", "b\r", "c"]);
    }

    #[test]
    fn a_reader_held_at_the_end_by_a_resize_is_not_carried_by_new_content() {
        let mut s = HistoryState::new();
        for i in 0..6 {
            s.push(format!("{i}: aaaa bbbb cccc dddd"));
        }
        draw(&s, 8, 3);
        s.top.set(Some(Pos { entry: 4, row: 0 }));
        // At this width all of it fits, so the anchor is past what can be
        // shown and the view is held at the last screenful.
        let held = draw(&s, 60, 3);
        assert!(!s.is_following());
        s.push("new 0");
        s.push("new 1");
        assert_eq!(draw(&s, 60, 3), held);
    }

    #[test]
    fn a_huge_entry_costs_what_is_visible_not_what_it_holds() {
        use crate::widgets::paragraph::PIECES;
        let pieces = |f: &mut dyn FnMut()| {
            PIECES.with(|c| c.set(0));
            f();
            PIECES.with(std::cell::Cell::get)
        };
        let mut s = HistoryState::new();
        s.push("agent:");
        s.append("a line of streamed text\n".repeat(50_000));
        // Counting it after a change of width is paid once.
        let first = pieces(&mut || {
            draw(&s, 10, 20);
        });
        assert!(first >= 50_000, "{first}");
        // Drawing again, following the end or somewhere inside, only looks
        // at the lines it draws.
        let again = pieces(&mut || {
            draw(&s, 10, 20);
        });
        assert!(again <= 25, "a repeated frame split {again} lines");
        s.scroll_up(30_000);
        let inside = pieces(&mut || {
            draw(&s, 10, 20);
        });
        assert!(inside <= 25, "a frame inside split {inside} lines");
        s.scroll_to_bottom();
        // A token: one line is counted again, and the frame draws the end.
        for _ in 0..50 {
            s.append("tok ");
            let n = pieces(&mut || {
                draw(&s, 10, 20);
            });
            assert!(n <= 30, "a streamed token split {n} lines");
        }
        // The whole entry counts as its lines do, one by one.
        let all: usize = s
            .entry(0)
            .unwrap()
            .lines
            .iter()
            .map(|l| line_rows(l, 10, Wrap::Word))
            .sum();
        assert_eq!(s.content_rows(), all);
    }

    /// Streamed chunks, cut anywhere, with the width changing under them,
    /// give what a paragraph of the same text gives.
    #[test]
    fn streaming_gives_what_a_paragraph_of_the_whole_text_gives() {
        let chunks = [
            "a",
            "bb ",
            "ccc\n",
            "\n",
            "dddddddddd",
            "中文",
            " ",
            "x y z\n",
            "😀",
            "long words here ",
        ];
        let mut seed = 0xfeed_beef_1234_5678u64;
        let mut next = move |m: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m as u64) as usize
        };
        for round in 0..150 {
            let wrap = [Wrap::Word, Wrap::Char, Wrap::None][round % 3];
            let mut s = HistoryState::new();
            let mut entries: Vec<String> = Vec::new();
            let mut w = (next(10) + 1) as u16;
            let h = (next(6) + 1) as u16;
            for step in 0..40 {
                match next(8) {
                    0 => {
                        let text = chunks[next(chunks.len())].to_string();
                        s.push(text.as_str());
                        entries.push(text);
                    }
                    1 => w = (next(10) + 1) as u16,
                    2 => s.scroll_up(next(5) + 1),
                    3 => s.scroll_down(next(5) + 1),
                    _ => {
                        let chunk = chunks[next(chunks.len())];
                        s.append(chunk);
                        match entries.last_mut() {
                            Some(last) => last.push_str(chunk),
                            None => entries.push(chunk.to_string()),
                        }
                    }
                }
                let all = Text {
                    lines: entries
                        .iter()
                        .flat_map(|e| Text::raw(e.as_str()).lines)
                        .collect(),
                    style: Style::new(),
                };
                let shown = draw_with(History::new().wrap(wrap), &s, w, h);
                let total: usize = all
                    .lines
                    .iter()
                    .map(|l| line_rows(l, usize::from(w), wrap))
                    .sum();
                assert_eq!(s.content_rows(), total, "round {round} step {step}");
                let area = Rect::new(0, 0, w, h);
                let mut buf = Buffer::new(area);
                Paragraph::new(all)
                    .wrap(wrap)
                    .scroll(s.position())
                    .render(area, &mut buf);
                assert_eq!(shown, rows_of(&buf), "round {round} step {step}");
            }
        }
    }
}

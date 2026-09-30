use std::cell::Cell;

use unicode_segmentation::UnicodeSegmentation;

use super::input::{is_space, next_boundary, prev_boundary, snap};
use super::{Block, StatefulWidget, Widget};
use crate::input::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crate::text::{grapheme_width, truncate, width};
use crate::{Buffer, Rect, Style};

/// Spaces a pasted tab becomes.
const TAB: &str = "    ";

/// One screen row of the text: the bytes `start..end` of it, without the
/// newline that ends the line, and their width in columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Row {
    start: usize,
    end: usize,
    width: usize,
}

/// Splits `text` into the rows it takes at `cols` columns. Each line is cut
/// at the column where the next cluster would not fit, so a row never ends
/// inside a cluster, and a glyph wider than the whole row still gets a row.
/// An empty line is one empty row. When `cursor` sits at the end of a line
/// whose last row is full, that line gets one more empty row for it, since
/// the cursor is drawn in a cell of its own.
fn layout(text: &str, cols: usize, cursor: Option<usize>) -> Vec<Row> {
    let cols = cols.max(1);
    let mut rows = Vec::new();
    let mut line_start = 0;
    for line in text.split('\n') {
        let mut start = line_start;
        let mut used = 0;
        for (i, g) in line.grapheme_indices(true) {
            let at = line_start + i;
            let w = grapheme_width(g);
            if used + w > cols && at > start {
                rows.push(Row {
                    start,
                    end: at,
                    width: used,
                });
                start = at;
                used = 0;
            }
            used += w;
        }
        let end = line_start + line.len();
        rows.push(Row {
            start,
            end,
            width: used,
        });
        if used >= cols && cursor == Some(end) {
            rows.push(Row {
                start: end,
                end,
                width: 0,
            });
        }
        line_start = end + 1;
    }
    rows
}

/// The row the cursor is on and its column in it. The cursor at the start of
/// a wrapped row belongs to that row, not to the end of the one before.
fn locate(rows: &[Row], cursor: usize, text: &str) -> (usize, usize) {
    let index = rows
        .partition_point(|r| r.start <= cursor)
        .saturating_sub(1);
    let row = rows[index];
    (index, width(&text[row.start..cursor.min(row.end)]))
}

/// The multi-line text of a [`TextArea`] and where the cursor is in it.
///
/// Editing works on grapheme clusters, like [`InputState`](super::InputState):
/// the cursor is always on a cluster boundary, and one press of backspace
/// removes one cluster. Line breaks are `\n` only. A pasted `\r\n` or `\r`
/// becomes `\n`, a tab becomes four spaces, and any other control character
/// is dropped.
///
/// Enter is not an editing key here. The app decides which keys submit and
/// which add a line, and calls [`TextAreaState::insert_newline`] for the
/// latter, so [`TextAreaState::handle_key`] leaves Enter, Tab and Ctrl+J to
/// it.
///
/// Drawing remembers the width it wrapped at, the vertical scroll and where
/// the cursor was drawn, which is why the widget only needs `&TextAreaState`.
/// Up and Down move by screen row, so they follow the wrapping the last draw
/// used, and by line before anything was drawn.
#[derive(Debug, Clone, Default)]
pub struct TextAreaState {
    text: String,
    cursor: usize,
    /// The column Up and Down try to keep, and the cursor they left it at.
    /// It stops applying as soon as the cursor is anywhere else.
    goal: Cell<Option<(usize, usize)>>,
    wrap_width: Cell<u16>,
    scroll: Cell<usize>,
    cursor_position: Cell<Option<(u16, u16)>>,
}

/// Two states are equal when they hold the same text with the cursor in the
/// same place. What drawing remembered is not part of it.
impl PartialEq for TextAreaState {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.cursor == other.cursor
    }
}

impl Eq for TextAreaState {}

impl TextAreaState {
    /// An empty text area.
    pub fn new() -> Self {
        TextAreaState::default()
    }

    /// A text area holding `text`, with the cursor at the end.
    pub fn with_text(text: impl AsRef<str>) -> Self {
        let mut state = TextAreaState::new();
        state.insert_str(text.as_ref());
        state
    }

    /// The text, with `\n` between lines.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when there is no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The number of lines, not counting wrapping. An empty text is one line.
    pub fn line_count(&self) -> usize {
        self.text.split('\n').count()
    }

    /// The cursor as a byte index into [`TextAreaState::text`], always on a
    /// character boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Where the cursor was drawn last time, in buffer cells, for
    /// `Frame::set_cursor`. `None` before the first draw and when there is no
    /// room. [`Frame::render_stateful_widget`](crate::Frame::render_stateful_widget)
    /// does this by itself.
    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        self.cursor_position.get()
    }

    /// How many rows the text takes at `width` columns, wrapped and with room
    /// for the cursor, but at least one and at most `max`. The app calls it
    /// to size the area before drawing, so the box grows with what is typed
    /// until it reaches `max` rows and scrolls from there.
    ///
    /// `width` is the width the text is drawn at. A [`TextArea`] with a block
    /// has less room than its area; [`TextArea::desired_height`] counts it.
    pub fn desired_height(&self, width: u16, max: u16) -> u16 {
        let rows = layout(&self.text, usize::from(width), Some(self.cursor)).len();
        u16::try_from(rows).unwrap_or(u16::MAX).max(1).min(max)
    }

    /// Replaces the text and moves the cursor to its end.
    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.clear();
        self.insert_str(text.as_ref());
    }

    /// Removes all the text.
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.scroll.set(0);
    }

    /// Inserts a character at the cursor. A `\n` starts a new line; other
    /// control characters are ignored, and a tab becomes four spaces.
    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    /// Inserts text at the cursor, which is how a paste goes in. Line breaks
    /// are kept (`\r\n` and `\r` become `\n`), tabs become four spaces, and
    /// other control characters are dropped.
    pub fn insert_str(&mut self, text: impl AsRef<str>) {
        let mut clean = String::new();
        let mut chars = text.as_ref().chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\r' => {
                    chars.next_if_eq(&'\n');
                    clean.push('\n');
                }
                '\n' => clean.push('\n'),
                '\t' => clean.push_str(TAB),
                c if c.is_control() => {}
                c => clean.push(c),
            }
        }
        if clean.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        // Inserted text can join the cluster after it, so the end may not be
        // a boundary.
        self.cursor = snap(&self.text, self.cursor + clean.len());
    }

    /// Inserts a line break at the cursor. The app calls this for the keys it
    /// maps to it, such as Shift+Enter.
    pub fn insert_newline(&mut self) {
        self.insert_str("\n");
    }

    /// Deletes the cluster before the cursor, which joins two lines when it
    /// is at the start of one. Returns whether anything went.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = prev_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = snap(&self.text, start);
        true
    }

    /// Deletes the cluster under the cursor, which joins two lines when it
    /// is at the end of one. Returns whether anything went.
    pub fn delete(&mut self) -> bool {
        if self.cursor == self.text.len() {
            return false;
        }
        let end = next_boundary(&self.text, self.cursor);
        self.text.replace_range(self.cursor..end, "");
        self.cursor = snap(&self.text, self.cursor);
        true
    }

    /// Moves one cluster left, over a line break at the start of a line.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_boundary(&self.text, self.cursor);
        }
    }

    /// Moves one cluster right, over a line break at the end of a line.
    pub fn move_right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
    }

    fn word_left_of(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 {
            let p = prev_boundary(&self.text, i);
            if !is_space(&self.text[p..i]) {
                break;
            }
            i = p;
        }
        while i > 0 {
            let p = prev_boundary(&self.text, i);
            if is_space(&self.text[p..i]) {
                break;
            }
            i = p;
        }
        i
    }

    /// Moves to the start of the word before the cursor, over line breaks. A
    /// word is a run of anything that isn't whitespace.
    pub fn move_word_left(&mut self) {
        self.cursor = self.word_left_of(self.cursor);
    }

    /// Moves to the end of the word after the cursor, over line breaks.
    pub fn move_word_right(&mut self) {
        let mut i = self.cursor;
        while i < self.text.len() {
            let n = next_boundary(&self.text, i);
            if !is_space(&self.text[i..n]) {
                break;
            }
            i = n;
        }
        while i < self.text.len() {
            let n = next_boundary(&self.text, i);
            if is_space(&self.text[i..n]) {
                break;
            }
            i = n;
        }
        self.cursor = i;
    }

    /// Where the line the cursor is on starts.
    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map_or(0, |i| i + 1)
    }

    /// Where the line the cursor is on ends, before its line break.
    fn line_end(&self) -> usize {
        self.text[self.cursor..]
            .find('\n')
            .map_or(self.text.len(), |i| self.cursor + i)
    }

    /// Moves to the start of the line, which is the whole line, not the
    /// screen row it wrapped to.
    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }

    /// Moves to the end of the line, which is the whole line, not the screen
    /// row it wrapped to.
    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }

    /// Moves to the start of the text.
    pub fn move_to_start(&mut self) {
        self.cursor = 0;
    }

    /// Moves to the end of the text.
    pub fn move_to_end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Moves to the same column one screen row up, or as far as that row
    /// goes. Returns false, and stays put, when the cursor is already on the
    /// first row, so the app can use the key for something else there, such
    /// as recalling an earlier message.
    pub fn move_up(&mut self) -> bool {
        self.move_rows(false)
    }

    /// Moves to the same column one screen row down, or as far as that row
    /// goes. Returns false, and stays put, when the cursor is already on the
    /// last row.
    pub fn move_down(&mut self) -> bool {
        self.move_rows(true)
    }

    fn move_rows(&mut self, down: bool) -> bool {
        let cols = match self.wrap_width.get() {
            0 => usize::MAX,
            w => usize::from(w),
        };
        let rows = layout(&self.text, cols, Some(self.cursor));
        let (index, column) = locate(&rows, self.cursor, &self.text);
        let target = if down {
            index + 1
        } else {
            match index.checked_sub(1) {
                Some(t) => t,
                None => return false,
            }
        };
        let Some(&row) = rows.get(target) else {
            return false;
        };
        let goal = match self.goal.get() {
            Some((at, goal)) if at == self.cursor => goal,
            _ => column,
        };
        // On a row that wraps on, the cursor can't be past its last cluster:
        // that cell belongs to the start of the next row.
        let wraps_on = self.text[row.end..]
            .chars()
            .next()
            .is_some_and(|c| c != '\n');
        let mut at = row.start;
        let mut used = 0;
        for (i, g) in self.text[row.start..row.end].grapheme_indices(true) {
            let w = grapheme_width(g);
            if used + w > goal {
                break;
            }
            used += w;
            at = row.start + i + g.len();
        }
        if wraps_on && at == row.end && row.end > row.start {
            at = prev_boundary(&self.text, row.end);
        }
        self.cursor = at;
        self.goal.set(Some((at, goal)));
        true
    }

    /// Deletes from the cursor to the end of the line, or the line break
    /// when there is nothing left on it. Returns whether anything went.
    pub fn kill_to_end(&mut self) -> bool {
        let mut end = self.line_end();
        if end == self.cursor {
            end = (end + 1).min(self.text.len());
        }
        let had = end > self.cursor;
        self.text.replace_range(self.cursor..end, "");
        self.cursor = snap(&self.text, self.cursor);
        had
    }

    /// Deletes from the start of the line to the cursor, or the line break
    /// before it when the cursor is at the start. Returns whether anything
    /// went.
    pub fn kill_to_start(&mut self) -> bool {
        let mut start = self.line_start();
        if start == self.cursor {
            start = start.saturating_sub(1);
        }
        let had = start < self.cursor;
        self.text.replace_range(start..self.cursor, "");
        self.cursor = snap(&self.text, start);
        had
    }

    /// Deletes the word before the cursor. Returns whether anything went.
    pub fn delete_word_back(&mut self) -> bool {
        let start = self.word_left_of(self.cursor);
        if start == self.cursor {
            return false;
        }
        self.text.replace_range(start..self.cursor, "");
        self.cursor = snap(&self.text, start);
        true
    }

    /// Applies an editing key and returns whether it was one, which is how a
    /// caller knows to pass any other key on to the rest of the app.
    ///
    /// It takes what [`InputState::handle_key`](super::InputState::handle_key)
    /// takes, plus Up and Down, which move between rows, and Ctrl+Home and
    /// Ctrl+End for the ends of the text. Home and End, Ctrl+A and Ctrl+E,
    /// and Ctrl+K and Ctrl+U work on the line.
    ///
    /// It does not take Enter, Tab or Ctrl+J, with any modifiers, so the app
    /// decides which of them submit and which call
    /// [`TextAreaState::insert_newline`]. Up and Down are left to the app
    /// too when the cursor is already on the first or the last row.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // A key that comes up has done its work when it went down.
        if key.kind == KeyEventKind::Release {
            return true;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CTRL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let sup = key.modifiers.contains(KeyModifiers::SUPER);
        match key.code {
            KeyCode::Char('j') if ctrl => return false,
            KeyCode::Char(c) if !ctrl && !alt && !sup => self.insert_char(c),
            KeyCode::Char('a') if ctrl => self.home(),
            KeyCode::Char('e') if ctrl => self.end(),
            KeyCode::Char('b') if ctrl => self.move_left(),
            KeyCode::Char('f') if ctrl => self.move_right(),
            KeyCode::Char('b') if alt => self.move_word_left(),
            KeyCode::Char('f') if alt => self.move_word_right(),
            KeyCode::Char('k') if ctrl => {
                self.kill_to_end();
            }
            KeyCode::Char('u') if ctrl => {
                self.kill_to_start();
            }
            KeyCode::Char('w') if ctrl => {
                self.delete_word_back();
            }
            KeyCode::Char('d') if ctrl => {
                self.delete();
            }
            KeyCode::Char('h') if ctrl => {
                self.backspace();
            }
            KeyCode::Backspace if alt => {
                self.delete_word_back();
            }
            KeyCode::Backspace => {
                self.backspace();
            }
            KeyCode::Delete => {
                self.delete();
            }
            KeyCode::Left if ctrl || alt => self.move_word_left(),
            KeyCode::Right if ctrl || alt => self.move_word_right(),
            KeyCode::Left => self.move_left(),
            KeyCode::Right => self.move_right(),
            KeyCode::Up => return self.move_up(),
            KeyCode::Down => return self.move_down(),
            KeyCode::Home if ctrl => self.move_to_start(),
            KeyCode::End if ctrl => self.move_to_end(),
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }
}

/// A multi-line text field that wraps long lines to its width.
///
/// ```
/// use crewtui::widgets::{StatefulWidget, TextArea, TextAreaState};
/// use crewtui::{Buffer, Rect};
///
/// let mut state = TextAreaState::with_text("hello");
/// state.insert_newline();
/// state.insert_str("world");
/// let area = Rect::new(0, 0, 10, 4);
/// let mut buf = Buffer::new(area);
/// TextArea::new().render(area, &mut buf, &state);
/// assert_eq!(state.cursor_position(), Some((5, 1)));
/// ```
///
/// Lines wrap at the column where the next character would not fit, not at
/// words. When the text takes more rows than the area has, it scrolls just
/// far enough to keep the cursor in view, and the scroll is remembered
/// between frames. Use `Frame::render_stateful_widget`, which also places the
/// terminal cursor at the text cursor. To make the area as tall as the text
/// needs, ask [`TextArea::desired_height`] before choosing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea<'a> {
    style: Style,
    placeholder: &'a str,
    placeholder_style: Style,
    block: Option<Block<'a>>,
}

impl<'a> TextArea<'a> {
    /// A text area with no placeholder and no block.
    pub fn new() -> Self {
        TextArea {
            style: Style::new(),
            placeholder: "",
            placeholder_style: Style::new().dim(),
            block: None,
        }
    }

    /// The style of the text and of the whole area.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Text shown while the text area is empty, cut at the edge. It is
    /// borrowed for the frame, so a `String` in the app's state works. Line
    /// breaks in it are kept.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// The style of the placeholder, over the text area's. Dim by default.
    pub fn placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }

    /// A block drawn around the text area.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The height to give the area so `state` fits at `width` columns wide,
    /// counting the block's borders and padding, and at most `max` rows.
    /// This is [`TextAreaState::desired_height`] for the room inside the
    /// block.
    pub fn desired_height(&self, state: &TextAreaState, width: u16, max: u16) -> u16 {
        let outer = Rect::new(0, 0, width, u16::MAX);
        let inner = self.block.as_ref().map_or(outer, |b| b.inner(outer));
        let chrome = u16::MAX - inner.height;
        let text = state.desired_height(inner.width, max.saturating_sub(chrome));
        text.saturating_add(chrome).min(max)
    }
}

impl Default for TextArea<'_> {
    fn default() -> Self {
        TextArea::new()
    }
}

impl StatefulWidget for TextArea<'_> {
    type State = TextAreaState;

    fn cursor(state: &TextAreaState) -> Option<(u16, u16)> {
        state.cursor_position()
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &TextAreaState) {
        state.cursor_position.set(None);
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
        state.wrap_width.set(area.width);
        let cols = usize::from(area.width);
        let height = usize::from(area.height);

        if state.text.is_empty() {
            let style = self.style.patch(self.placeholder_style);
            for (i, line) in self.placeholder.split('\n').take(height).enumerate() {
                buf.set_string(area.x, area.y + i as u16, truncate(line, cols), style);
            }
            state.scroll.set(0);
            state.cursor_position.set(Some((area.x, area.y)));
            return;
        }

        let rows = layout(&state.text, cols, Some(state.cursor));
        let (cursor_row, cursor_col) = locate(&rows, state.cursor, &state.text);
        // Scroll only as far as it takes to keep the cursor row in view.
        let mut scroll = state.scroll.get().min(rows.len().saturating_sub(height));
        if cursor_row < scroll {
            scroll = cursor_row;
        } else if cursor_row >= scroll + height {
            scroll = cursor_row + 1 - height;
        }
        state.scroll.set(scroll);

        for (i, row) in rows.iter().skip(scroll).take(height).enumerate() {
            let y = area.y + i as u16;
            buf.set_string(area.x, y, &state.text[row.start..row.end], self.style);
        }
        let x = area.x + cursor_col.min(cols - 1) as u16;
        state
            .cursor_position
            .set(Some((x, area.y + (cursor_row - scroll) as u16)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::Borders;
    use crate::{Color, Frame};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn plain(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), KeyModifiers::CTRL)
    }

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

    fn draw(area: TextArea<'_>, state: &TextAreaState, w: u16, h: u16) -> Vec<String> {
        let rect = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(rect);
        area.render(rect, &mut buf, state);
        rows_of(&buf)
    }

    fn show(state: &TextAreaState, w: u16, h: u16) -> Vec<String> {
        draw(TextArea::new(), state, w, h)
    }

    const FAMILY: &str = "👨‍👩‍👧‍👦";

    #[test]
    fn typing_and_newlines_build_lines() {
        let mut s = TextAreaState::new();
        for c in "ab".chars() {
            assert!(s.handle_key(plain(c)));
        }
        s.insert_newline();
        s.insert_str("cd");
        assert_eq!(s.text(), "ab\ncd");
        assert_eq!(s.line_count(), 2);
        assert_eq!(s.cursor(), 5);
        assert_eq!(show(&s, 4, 3), vec!["ab  ", "cd  ", "    "]);
        assert_eq!(s.cursor_position(), Some((2, 1)));
    }

    #[test]
    fn enter_tab_and_ctrl_j_are_left_to_the_app() {
        let mut s = TextAreaState::with_text("x");
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
            KeyModifiers::CTRL,
        ] {
            assert!(!s.handle_key(key(KeyCode::Enter, modifiers)));
        }
        assert!(!s.handle_key(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(!s.handle_key(ctrl('j')));
        assert_eq!(s.text(), "x");
    }

    #[test]
    fn paste_keeps_line_breaks_and_normalizes_the_rest() {
        let mut s = TextAreaState::with_text("ac");
        s.move_left();
        s.insert_str("b\r\nc\rd\te\u{1b}[31m\u{7}");
        assert_eq!(s.text(), "ab\nc\nd    e[31mc");
        assert_eq!(s.cursor(), s.text().len() - 1);
        assert_eq!(TextAreaState::with_text("a\r\n").text(), "a\n");
    }

    #[test]
    fn backspace_and_delete_join_lines_and_remove_whole_clusters() {
        let mut s = TextAreaState::with_text("ab\ncd");
        s.home();
        assert!(s.backspace());
        assert_eq!((s.text(), s.cursor()), ("abcd", 2));
        let mut s = TextAreaState::with_text("ab\ncd");
        s.move_to_start();
        s.end();
        assert!(s.delete());
        assert_eq!(s.text(), "abcd");
        for cluster in ["é", "e\u{301}", "中", "😀", "👍🏽", FAMILY, "🇧🇷"] {
            let mut s = TextAreaState::with_text(format!("a\n{cluster}\nb"));
            s.move_left();
            s.move_left();
            assert!(s.backspace(), "{cluster}");
            assert_eq!(s.text(), "a\n\nb", "{cluster}");
        }
    }

    #[test]
    fn home_and_end_work_on_the_line_not_the_wrapped_row() {
        let mut s = TextAreaState::with_text("one\ntwo three");
        show(&s, 4, 5);
        s.home();
        assert_eq!(s.cursor(), 4);
        s.end();
        assert_eq!(s.cursor(), s.text().len());
        // Up goes to the wrapped row "thre", and End still reaches the end
        // of the whole line.
        s.move_up();
        s.end();
        assert_eq!(s.cursor(), s.text().len());
        s.move_to_start();
        s.end();
        assert_eq!(s.cursor(), 3);
        s.move_to_start();
        assert_eq!(s.cursor(), 0);
        s.move_to_end();
        assert_eq!(s.cursor(), s.text().len());
    }

    #[test]
    fn up_and_down_keep_the_column_across_lines_of_different_length() {
        let mut s = TextAreaState::with_text("abcdef\nab\nabcdef");
        assert_eq!(s.cursor(), s.text().len());
        s.home();
        for _ in 0..5 {
            s.move_right();
        }
        assert!(s.move_up());
        // The short line stops the cursor at its end...
        assert_eq!(s.cursor(), "abcdef\nab".len());
        assert!(s.move_up());
        // ...and the column it wanted is still remembered.
        assert_eq!(s.cursor(), 5);
        assert!(!s.move_up());
        assert_eq!(s.cursor(), 5);
        assert!(s.move_down() && s.move_down());
        assert_eq!(s.cursor(), "abcdef\nab\nabcde".len());
        assert!(!s.move_down());
    }

    #[test]
    fn the_remembered_column_is_dropped_when_the_cursor_moves_another_way() {
        let mut s = TextAreaState::with_text("abcdef\nab\nabcdef");
        s.home();
        for _ in 0..5 {
            s.move_right();
        }
        s.move_up();
        s.move_left();
        s.move_up();
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn up_and_down_use_display_columns_with_wide_glyphs() {
        let mut s = TextAreaState::with_text("中文字\nabcdef");
        // On "abcdef" at column 3, which is inside the second wide glyph.
        s.home();
        for _ in 0..3 {
            s.move_right();
        }
        s.move_up();
        assert_eq!(&s.text()[s.cursor()..], "文字\nabcdef");
        s.move_down();
        assert_eq!(s.cursor(), "中文字\nabc".len());
        // Column 2 of the first line is a glyph boundary.
        s.move_left();
        s.move_up();
        assert_eq!(&s.text()[s.cursor()..], "文字\nabcdef");
    }

    #[test]
    fn up_and_down_move_by_wrapped_row_once_drawn() {
        let mut s = TextAreaState::with_text("abcdefgh");
        show(&s, 3, 4);
        // Rows: abc / def / gh, and the cursor is at the end of the last.
        assert_eq!(s.cursor_position(), Some((2, 2)));
        assert!(s.move_up());
        assert_eq!(s.cursor(), 5);
        assert!(s.move_up());
        assert_eq!(s.cursor(), 2);
        assert!(!s.move_up());
        assert!(s.move_down());
        assert_eq!(s.cursor(), 5);
    }

    #[test]
    fn a_wrapped_row_never_takes_the_cursor_onto_the_next_row() {
        let mut s = TextAreaState::with_text("abcdefgh\nxy");
        show(&s, 4, 5);
        // Rows: abcd / efgh / xy. From column 3 of "xy"'s row the cursor
        // goes up to the last cell of "efgh", not to the start of "xy".
        s.move_to_end();
        s.move_left();
        s.move_left();
        s.move_right();
        s.move_right();
        assert!(s.move_up());
        assert_eq!(s.cursor(), "abcdefgh".len() - 2);
    }

    #[test]
    fn word_movement_and_deletion_cross_lines() {
        let mut s = TextAreaState::with_text("one two\nthree four");
        s.move_word_left();
        s.move_word_left();
        assert_eq!(&s.text()[s.cursor()..], "three four");
        s.move_word_left();
        assert_eq!(&s.text()[s.cursor()..], "two\nthree four");
        s.move_word_right();
        assert_eq!(&s.text()[s.cursor()..], "\nthree four");
        assert!(s.delete_word_back());
        assert_eq!(s.text(), "one \nthree four");
        assert!(s.handle_key(key(KeyCode::Backspace, KeyModifiers::ALT)));
        assert_eq!(s.text(), "\nthree four");
        assert!(!s.delete_word_back());
    }

    #[test]
    fn kill_works_on_the_line_and_then_on_the_line_break() {
        let mut s = TextAreaState::with_text("ab\ncd\nef");
        s.move_up();
        s.home();
        s.move_right();
        assert!(s.kill_to_end());
        assert_eq!(s.text(), "ab\nc\nef");
        assert!(s.kill_to_end());
        assert_eq!(s.text(), "ab\ncef");
        assert!(s.kill_to_start());
        assert_eq!((s.text(), s.cursor()), ("ab\nef", 3));
        assert!(s.kill_to_start());
        assert_eq!((s.text(), s.cursor()), ("abef", 2));
        s.move_to_end();
        assert!(!s.kill_to_end());
        s.move_to_start();
        assert!(!s.kill_to_start());
    }

    #[test]
    fn the_cursor_steps_over_one_cluster_and_over_line_breaks() {
        let mut s = TextAreaState::with_text(format!("a{FAMILY}\ne\u{301}"));
        let len = s.text().len();
        s.move_left();
        assert_eq!(s.cursor(), len - "e\u{301}".len());
        s.move_left();
        assert_eq!(s.cursor(), len - "e\u{301}".len() - 1);
        s.move_left();
        assert_eq!(s.cursor(), 1);
        s.move_right();
        s.move_right();
        s.move_right();
        assert_eq!(s.cursor(), len);
        s.move_right();
        assert_eq!(s.cursor(), len);
    }

    #[test]
    fn long_lines_wrap_at_the_width_and_keep_their_breaks() {
        let s = TextAreaState::with_text("abcdefg\n\nhi");
        assert_eq!(
            show(&s, 3, 6),
            vec!["abc", "def", "g  ", "   ", "hi ", "   "]
        );
    }

    #[test]
    fn wide_glyphs_wrap_whole_and_leave_the_edge_cell_blank() {
        let s = TextAreaState::with_text("a中文b");
        // "a中" takes 3 of 4 columns, and 文 doesn't fit in the last one.
        assert_eq!(show(&s, 4, 3), vec!["a中 ", "文b ", "    "]);
        let rect = Rect::new(0, 0, 4, 3);
        let mut buf = Buffer::new(rect);
        TextArea::new().render(rect, &mut buf, &s);
        assert!(buf.get(2, 0).unwrap().is_continuation());
        assert_eq!(buf.get(3, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn a_glyph_wider_than_the_area_still_gets_a_row_and_the_cursor_stays_inside() {
        let s = TextAreaState::with_text("中a");
        show(&s, 1, 3);
        let (x, y) = s.cursor_position().unwrap();
        assert!(x < 1 && y < 3);
        // 中, then a on its own full row, then the row for the cursor.
        assert_eq!(s.desired_height(1, 10), 3);
    }

    #[test]
    fn the_cursor_lands_in_the_right_cell_with_wide_and_combining_text() {
        let mut s = TextAreaState::with_text("ab\n中e\u{301}😀");
        show(&s, 10, 4);
        assert_eq!(s.cursor_position(), Some((2 + 1 + 2, 1)));
        s.move_left();
        show(&s, 10, 4);
        assert_eq!(s.cursor_position(), Some((3, 1)));
        s.move_left();
        s.move_left();
        show(&s, 10, 4);
        assert_eq!(s.cursor_position(), Some((0, 1)));
        s.move_left();
        show(&s, 10, 4);
        assert_eq!(s.cursor_position(), Some((2, 0)));
    }

    #[test]
    fn the_cursor_at_a_wrap_point_is_at_the_start_of_the_next_row() {
        let mut s = TextAreaState::with_text("abcdef");
        s.move_left();
        s.move_left();
        s.move_left();
        show(&s, 3, 4);
        assert_eq!(s.cursor(), 3);
        assert_eq!(s.cursor_position(), Some((0, 1)));
    }

    #[test]
    fn the_cursor_at_the_end_of_a_full_row_gets_a_row_of_its_own() {
        let mut s = TextAreaState::with_text("abc");
        assert_eq!(show(&s, 3, 3), vec!["abc", "   ", "   "]);
        assert_eq!(s.cursor_position(), Some((0, 1)));
        assert_eq!(s.desired_height(3, 9), 2);
        // Away from the end the extra row is not needed.
        s.move_left();
        assert_eq!(s.desired_height(3, 9), 1);
        // Also at the end of a full line that a newline follows.
        let mut s = TextAreaState::with_text("abc\nx");
        s.move_to_start();
        s.end();
        show(&s, 3, 4);
        assert_eq!(s.cursor_position(), Some((0, 1)));
        assert_eq!(s.desired_height(3, 9), 3);
    }

    #[test]
    fn desired_height_grows_with_the_text_and_is_clamped() {
        let mut s = TextAreaState::new();
        assert_eq!(s.desired_height(10, 5), 1);
        assert_eq!(s.desired_height(10, 0), 0);
        s.insert_str("a\nb\nc");
        assert_eq!(s.desired_height(10, 5), 3);
        assert_eq!(s.desired_height(10, 2), 2);
        s.insert_str("dddddddddddd");
        assert_eq!(s.desired_height(5, 20), 2 + 3);
        assert_eq!(s.desired_height(0, 20), s.desired_height(1, 20));
    }

    #[test]
    fn desired_height_of_the_widget_counts_the_block() {
        let s = TextAreaState::with_text("one\ntwo\nthree");
        let plain = TextArea::new();
        let boxed = TextArea::new().block(Block::new().borders(Borders::ALL));
        assert_eq!(plain.desired_height(&s, 20, 10), 3);
        assert_eq!(boxed.desired_height(&s, 20, 10), 5);
        assert_eq!(boxed.desired_height(&s, 20, 4), 4);
        // The borders take two columns, so this wraps where the plain one doesn't.
        let s = TextAreaState::with_text("abcd");
        assert_eq!(plain.desired_height(&s, 5, 10), 1);
        assert_eq!(boxed.desired_height(&s, 5, 10), 2 + 2);
    }

    #[test]
    fn a_tall_text_scrolls_to_keep_the_cursor_row_in_view() {
        let mut s = TextAreaState::with_text("1\n2\n3\n4\n5");
        assert_eq!(show(&s, 3, 2), vec!["4  ", "5  "]);
        assert_eq!(s.cursor_position(), Some((1, 1)));
        s.move_to_start();
        assert_eq!(show(&s, 3, 2), vec!["1  ", "2  "]);
        assert_eq!(s.cursor_position(), Some((0, 0)));
        s.move_down();
        s.move_down();
        assert_eq!(show(&s, 3, 2), vec!["2  ", "3  "]);
        assert_eq!(s.cursor_position(), Some((0, 1)));
        // Moving back up inside the view doesn't scroll.
        s.move_up();
        assert_eq!(show(&s, 3, 2), vec!["2  ", "3  "]);
        assert_eq!(s.cursor_position(), Some((0, 0)));
    }

    #[test]
    fn the_placeholder_shows_while_empty_with_the_cursor_at_the_start() {
        let s = TextAreaState::new();
        let out = draw(
            TextArea::new().placeholder("type here\nline two too long"),
            &s,
            8,
            3,
        );
        assert_eq!(out, vec!["type her", "line two", "        "]);
        assert_eq!(s.cursor_position(), Some((0, 0)));
        let rect = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::new(rect);
        TextArea::new().placeholder("x").render(rect, &mut buf, &s);
        assert!(
            buf.get(0, 0)
                .unwrap()
                .style()
                .modifiers
                .contains(crate::Modifier::DIM)
        );
        let typed = TextAreaState::with_text("a");
        assert_eq!(
            draw(TextArea::new().placeholder("x"), &typed, 3, 1),
            vec!["a  "]
        );
    }

    #[test]
    fn a_block_shrinks_the_room_and_moves_the_cursor_inside_it() {
        let s = TextAreaState::with_text("abcd\nef");
        let out = draw(
            TextArea::new().block(Block::new().borders(Borders::ALL)),
            &s,
            6,
            6,
        );
        assert_eq!(
            out,
            vec!["┌────┐", "│abcd│", "│ef  │", "│    │", "│    │", "└────┘"]
        );
        assert_eq!(s.cursor_position(), Some((3, 2)));
    }

    #[test]
    fn frame_places_the_terminal_cursor_at_the_text_cursor() {
        let s = TextAreaState::with_text("ab\ncd");
        let mut buf = Buffer::new(Rect::new(0, 0, 8, 4));
        let mut frame = Frame::new(&mut buf);
        frame.render_stateful_widget(TextArea::new(), Rect::new(2, 1, 5, 3), &s);
        assert_eq!(frame.cursor(), Some((4, 2)));
    }

    #[test]
    fn no_room_means_no_cursor_and_no_panic() {
        let s = TextAreaState::with_text("abc");
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 2));
        TextArea::new().render(Rect::new(0, 0, 0, 2), &mut buf, &s);
        assert_eq!(s.cursor_position(), None);
        TextArea::new().render(Rect::new(0, 0, 4, 0), &mut buf, &s);
        assert_eq!(s.cursor_position(), None);
        TextArea::new().render(Rect::new(10, 10, 4, 2), &mut buf, &s);
        assert_eq!(s.cursor_position(), None);
    }

    #[test]
    fn the_style_covers_the_area() {
        let s = TextAreaState::with_text("a");
        let rect = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::new(rect);
        TextArea::new()
            .style(Style::new().bg(Color::Blue))
            .render(rect, &mut buf, &s);
        assert!(
            buf.cells()
                .iter()
                .all(|c| c.style().bg == Some(Color::Blue))
        );
    }

    #[test]
    fn handle_key_maps_the_editing_keys_and_leaves_the_rest() {
        let mut s = TextAreaState::with_text("one two\nabc");
        assert!(s.handle_key(ctrl('a')));
        assert_eq!(s.cursor(), 8);
        assert!(s.handle_key(ctrl('e')));
        assert_eq!(s.cursor(), 11);
        assert!(s.handle_key(key(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(s.cursor(), 3);
        assert!(!s.handle_key(key(KeyCode::Up, KeyModifiers::NONE)));
        assert!(s.handle_key(key(KeyCode::Home, KeyModifiers::CTRL)));
        assert_eq!(s.cursor(), 0);
        assert!(s.handle_key(key(KeyCode::End, KeyModifiers::CTRL)));
        assert_eq!(s.cursor(), 11);
        assert!(!s.handle_key(key(KeyCode::Down, KeyModifiers::NONE)));
        assert!(s.handle_key(ctrl('u')));
        assert_eq!(s.text(), "one two\n");
        assert!(s.handle_key(key(KeyCode::Delete, KeyModifiers::NONE)));
        assert!(!s.handle_key(key(KeyCode::PageUp, KeyModifiers::NONE)));
        assert!(!s.handle_key(key(KeyCode::Char('c'), KeyModifiers::SUPER)));
        assert!(s.handle_key(
            key(KeyCode::Char('z'), KeyModifiers::NONE).with_kind(KeyEventKind::Release)
        ));
        assert_eq!(s.text(), "one two\n");
    }

    #[test]
    fn set_text_and_clear() {
        let mut s = TextAreaState::with_text("abc");
        s.set_text("中\n文");
        assert_eq!((s.text(), s.cursor()), ("中\n文", 7));
        s.clear();
        assert!(s.is_empty());
        assert_eq!((s.cursor(), s.line_count()), (0, 1));
    }

    #[test]
    fn edits_never_leave_the_cursor_inside_a_cluster() {
        // Inserting a letter before a combining mark joins them.
        let mut s = TextAreaState::with_text("\u{301}");
        s.move_to_start();
        s.insert_char('e');
        assert_eq!(s.text(), "e\u{301}");
        assert_eq!(s.cursor(), s.text().len());
    }
}

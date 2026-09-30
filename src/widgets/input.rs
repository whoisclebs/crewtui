use std::cell::Cell;

use unicode_segmentation::UnicodeSegmentation;

use super::{Block, StatefulWidget, Widget};
use crate::input::{KeyCode, KeyEvent, Modifiers};
use crate::text::{grapheme_width, truncate, width};
use crate::{Buffer, Rect, Style};

/// The text of a single-line input and where the cursor is in it.
///
/// Editing works on grapheme clusters, so an accented letter, an emoji with
/// a skin tone or a family emoji is one step for the cursor and one press of
/// backspace. The cursor is always on a cluster boundary, and its position on
/// screen is worked out from display width, so it lands in the right cell
/// with wide and combining characters. Control characters, including
/// newlines, never enter the text.
///
/// The app changes the state in `update`, usually by passing every key to
/// [`InputState::handle_key`] and every paste to [`InputState::insert_str`].
/// Drawing remembers the horizontal scroll and where the cursor was drawn in
/// cells, which is why the widget only needs `&InputState`.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    text: String,
    cursor: usize,
    scroll: Cell<usize>,
    cursor_position: Cell<Option<(u16, u16)>>,
}

/// Two states are equal when they hold the same text with the cursor in the
/// same place. What drawing remembered is not part of it.
impl PartialEq for InputState {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text && self.cursor == other.cursor
    }
}

impl Eq for InputState {}

/// Start of the cluster that ends at `at`.
fn prev_boundary(text: &str, at: usize) -> usize {
    text[..at]
        .grapheme_indices(true)
        .next_back()
        .map_or(0, |(i, _)| i)
}

/// End of the cluster that starts at `at`.
fn next_boundary(text: &str, at: usize) -> usize {
    text[at..]
        .graphemes(true)
        .next()
        .map_or(at, |g| at + g.len())
}

/// The first cluster boundary at or after `at`.
fn snap(text: &str, at: usize) -> usize {
    for (i, g) in text.grapheme_indices(true) {
        if at == i {
            return i;
        }
        if at < i + g.len() {
            return i + g.len();
        }
    }
    text.len()
}

fn is_space(g: &str) -> bool {
    g.chars().all(char::is_whitespace)
}

impl InputState {
    /// An empty input.
    pub fn new() -> Self {
        InputState::default()
    }

    /// An input holding `text`, with the cursor at the end.
    pub fn with_text(text: impl AsRef<str>) -> Self {
        let mut state = InputState::new();
        state.insert_str(text.as_ref());
        state
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// True when there is no text.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The cursor as a byte index into [`InputState::text`], always on a
    /// character boundary.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Screen columns from the start of the text to the cursor.
    pub fn cursor_column(&self) -> usize {
        width(&self.text[..self.cursor])
    }

    /// Where the cursor was drawn last time, in buffer cells, for
    /// `Frame::set_cursor`. `None` before the first draw and when the input
    /// has no room.
    pub fn cursor_position(&self) -> Option<(u16, u16)> {
        self.cursor_position.get()
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
    }

    /// Inserts a character at the cursor. A control character is ignored.
    pub fn insert_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.insert_str(c.encode_utf8(&mut buf));
    }

    /// Inserts text at the cursor, which is how a paste goes in. Control
    /// characters, newlines included, are dropped.
    pub fn insert_str(&mut self, text: &str) {
        let clean: String = text.chars().filter(|c| !c.is_control()).collect();
        if clean.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, &clean);
        // Inserted text can join the cluster after it, for instance a letter
        // before a combining mark, so the end may not be a boundary.
        self.cursor = snap(&self.text, self.cursor + clean.len());
    }

    /// Deletes the cluster before the cursor. Returns whether anything went.
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let start = prev_boundary(&self.text, self.cursor);
        self.text.replace_range(start..self.cursor, "");
        self.cursor = snap(&self.text, start);
        true
    }

    /// Deletes the cluster under the cursor. Returns whether anything went.
    pub fn delete(&mut self) -> bool {
        if self.cursor == self.text.len() {
            return false;
        }
        let end = next_boundary(&self.text, self.cursor);
        self.text.replace_range(self.cursor..end, "");
        // What is left on both sides can join into one cluster, such as two
        // regional indicators once the letter between them is gone.
        self.cursor = snap(&self.text, self.cursor);
        true
    }

    /// Moves one cluster left.
    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = prev_boundary(&self.text, self.cursor);
        }
    }

    /// Moves one cluster right.
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

    /// Moves to the start of the word before the cursor. A word is a run of
    /// anything that isn't whitespace.
    pub fn move_word_left(&mut self) {
        self.cursor = self.word_left_of(self.cursor);
    }

    /// Moves to the end of the word after the cursor.
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

    /// Moves to the start of the text.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Moves to the end of the text.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Deletes from the cursor to the end. Returns whether anything went.
    pub fn kill_to_end(&mut self) -> bool {
        let had = self.cursor < self.text.len();
        self.text.truncate(self.cursor);
        had
    }

    /// Deletes from the start to the cursor. Returns whether anything went.
    pub fn kill_to_start(&mut self) -> bool {
        let had = self.cursor > 0;
        self.text.replace_range(..self.cursor, "");
        self.cursor = 0;
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
    /// Characters insert. Left, Right, Home and End move, with Ctrl or Alt
    /// making Left and Right move by word; Backspace and Delete delete. The
    /// usual shell keys work too: Ctrl+A and Ctrl+E for the ends, Ctrl+B and
    /// Ctrl+F for one step, Alt+B and Alt+F for a word, Ctrl+K to delete to
    /// the end, Ctrl+U to the start, and Ctrl+W or Alt+Backspace for the word
    /// before the cursor.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(Modifiers::CTRL);
        let alt = key.modifiers.contains(Modifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => self.insert_char(c),
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
            KeyCode::Home => self.home(),
            KeyCode::End => self.end(),
            _ => return false,
        }
        true
    }
}

/// A single-line text field.
///
/// ```
/// use crewtui::widgets::{Input, InputState, StatefulWidget};
/// use crewtui::{Buffer, Rect};
///
/// let mut state = InputState::new();
/// state.insert_str("hello");
/// let area = Rect::new(0, 0, 10, 1);
/// let mut buf = Buffer::new(area);
/// Input::new().render(area, &mut buf, &state);
/// assert_eq!(state.cursor_position(), Some((5, 0)));
/// ```
///
/// Text wider than the area scrolls sideways just far enough to keep the
/// cursor in view, and the scroll is remembered between frames. Use
/// `Frame::render_input` to draw it and place the terminal cursor in one
/// call. Only the first row of the area is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input<'a> {
    style: Style,
    placeholder: &'a str,
    placeholder_style: Style,
    block: Option<Block<'a>>,
}

impl<'a> Input<'a> {
    /// An input with no placeholder and no block.
    pub fn new() -> Self {
        Input {
            style: Style::new(),
            placeholder: "",
            placeholder_style: Style::new().dim(),
            block: None,
        }
    }

    /// The style of the text and of the row it sits on.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Text shown while the input is empty.
    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    /// The style of the placeholder, over the input's. Dim by default.
    pub fn placeholder_style(mut self, style: Style) -> Self {
        self.placeholder_style = style;
        self
    }

    /// A block drawn around the input.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }
}

impl Default for Input<'_> {
    fn default() -> Self {
        Input::new()
    }
}

impl StatefulWidget for Input<'_> {
    type State = InputState;

    fn render(self, area: Rect, buf: &mut Buffer, state: &InputState) {
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
        let row = Rect { height: 1, ..area };
        buf.set_style(row, self.style);
        let room = usize::from(area.width);

        if state.text.is_empty() {
            let style = self.style.patch(self.placeholder_style);
            buf.set_string(area.x, area.y, truncate(self.placeholder, room), style);
            state.scroll.set(0);
            state.cursor_position.set(Some((area.x, area.y)));
            return;
        }

        // Scroll only as far as it takes to keep the cursor in view; the
        // cursor may sit one cell past the text, so the text plus one cell
        // is what the scroll can reach.
        let cursor_col = state.cursor_column();
        let end_col = width(&state.text);
        let mut scroll = state.scroll.get().min((end_col + 1).saturating_sub(room));
        if cursor_col < scroll {
            scroll = cursor_col;
        } else if cursor_col >= scroll + room {
            scroll = cursor_col + 1 - room;
        }
        state.scroll.set(scroll);

        let mut col = 0;
        for g in state.text.graphemes(true) {
            let w = grapheme_width(g);
            if w == 0 {
                continue;
            }
            let end = col + w;
            if end <= scroll {
                col = end;
                continue;
            }
            if col < scroll {
                // A wide glyph cut by the left edge: what shows of it is blank.
                let shown = end - scroll;
                for k in 0..shown.min(room) {
                    buf.set_string(area.x + k as u16, area.y, " ", self.style);
                }
            } else if end - scroll <= room {
                buf.set_string(area.x + (col - scroll) as u16, area.y, g, self.style);
            } else {
                break;
            }
            col = end;
        }
        let x = area.x + (cursor_col - scroll) as u16;
        state.cursor_position.set(Some((x, area.y)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Color, Frame};

    fn key(code: KeyCode, modifiers: Modifiers) -> KeyEvent {
        KeyEvent { code, modifiers }
    }

    fn plain(c: char) -> KeyEvent {
        key(KeyCode::Char(c), Modifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        key(KeyCode::Char(c), Modifiers::CTRL)
    }

    fn alt(c: char) -> KeyEvent {
        key(KeyCode::Char(c), Modifiers::ALT)
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

    fn draw(input: Input<'_>, state: &InputState, w: u16) -> String {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::new(area);
        input.render(area, &mut buf, state);
        rows_of(&buf).remove(0)
    }

    const FAMILY: &str = "👨‍👩‍👧‍👦";

    #[test]
    fn typing_inserts_at_the_cursor() {
        let mut s = InputState::new();
        for c in "hello".chars() {
            assert!(s.handle_key(plain(c)));
        }
        assert_eq!((s.text(), s.cursor()), ("hello", 5));
        s.move_left();
        s.move_left();
        s.insert_char('X');
        assert_eq!((s.text(), s.cursor()), ("helXlo", 4));
    }

    #[test]
    fn backspace_and_delete_remove_whole_clusters() {
        for cluster in [
            "é",
            "e\u{301}",
            "中",
            "😀",
            "👍🏽",
            FAMILY,
            "🇧🇷",
            "a\u{301}\u{302}",
        ] {
            let mut s = InputState::with_text(format!("a{cluster}b"));
            s.move_left();
            assert!(s.backspace(), "{cluster}");
            assert_eq!(s.text(), "ab", "{cluster}");
            assert_eq!(s.cursor(), 1);
            let mut s = InputState::with_text(format!("a{cluster}b"));
            s.home();
            s.move_right();
            assert!(s.delete());
            assert_eq!(s.text(), "ab", "{cluster}");
            assert_eq!(s.cursor(), 1);
        }
    }

    #[test]
    fn the_cursor_steps_over_one_cluster_at_a_time_and_stops_at_the_ends() {
        let mut s = InputState::with_text(format!("a{FAMILY}e\u{301}"));
        let mut stops = vec![s.cursor()];
        for _ in 0..5 {
            s.move_left();
            stops.push(s.cursor());
        }
        let len = s.text().len();
        assert_eq!(stops, vec![len, len - "e\u{301}".len(), 1, 0, 0, 0]);
        s.move_right();
        s.move_right();
        s.move_right();
        s.move_right();
        assert_eq!(s.cursor(), len);
        assert!(!s.delete());
        s.home();
        assert!(!s.backspace());
    }

    #[test]
    fn the_cursor_column_counts_display_width() {
        let mut s = InputState::with_text("a中😀é");
        assert_eq!(s.cursor_column(), 1 + 2 + 2 + 1);
        s.move_left();
        assert_eq!(s.cursor_column(), 5);
        s.home();
        assert_eq!(s.cursor_column(), 0);
    }

    #[test]
    fn inserting_before_a_combining_mark_keeps_the_cursor_on_a_boundary() {
        let mut s = InputState::with_text("\u{301}");
        s.home();
        s.insert_char('e');
        // The letter and the mark are one cluster, so the cursor is after both.
        assert_eq!(s.text(), "e\u{301}");
        assert_eq!(s.cursor(), s.text().len());
        // And a mark typed after a letter joins it.
        let mut s = InputState::with_text("e");
        s.insert_char('\u{301}');
        assert_eq!(s.cursor(), s.text().len());
        assert!(s.backspace());
        assert_eq!(s.text(), "");
    }

    #[test]
    fn word_movement_skips_whitespace_and_stops_at_word_edges() {
        let mut s = InputState::with_text("foo  bar-baz  qux");
        s.move_word_left();
        assert_eq!(s.cursor(), "foo  bar-baz  ".len());
        s.move_word_left();
        assert_eq!(s.cursor(), "foo  ".len());
        s.move_word_left();
        assert_eq!(s.cursor(), 0);
        s.move_word_left();
        assert_eq!(s.cursor(), 0);
        s.move_word_right();
        assert_eq!(s.cursor(), 3);
        s.move_word_right();
        assert_eq!(s.cursor(), "foo  bar-baz".len());
        s.move_word_right();
        s.move_word_right();
        assert_eq!(s.cursor(), s.text().len());
    }

    #[test]
    fn word_movement_works_with_wide_and_multi_scalar_words() {
        let mut s = InputState::with_text(format!("中文 {FAMILY}é x"));
        s.move_word_left();
        assert_eq!(s.text()[s.cursor()..].to_string(), "x");
        s.move_word_left();
        assert_eq!(s.text()[s.cursor()..].to_string(), format!("{FAMILY}é x"));
        s.move_word_left();
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn kill_and_delete_word() {
        let mut s = InputState::with_text("one two three");
        s.move_word_left();
        assert!(s.kill_to_end());
        assert_eq!(s.text(), "one two ");
        assert!(s.delete_word_back());
        assert_eq!(s.text(), "one ");
        s.insert_str("zz");
        s.home();
        s.move_right();
        assert!(s.kill_to_start());
        assert_eq!((s.text(), s.cursor()), ("ne zz", 0));
        assert!(!s.kill_to_start());
        s.end();
        assert!(!s.kill_to_end());
        s.home();
        assert!(!s.delete_word_back());
    }

    #[test]
    fn paste_inserts_text_and_drops_control_characters() {
        let mut s = InputState::with_text("ac");
        s.move_left();
        s.insert_str("b\r\n\tX\u{1b}[31m\u{7}");
        assert_eq!(s.text(), "abX[31mc");
        assert_eq!(s.cursor(), 7);
        s.insert_char('\n');
        s.insert_str("\n\n");
        assert_eq!(s.text(), "abX[31mc");
        assert_eq!(InputState::with_text("a\nb").text(), "ab");
    }

    #[test]
    fn set_text_and_clear() {
        let mut s = InputState::with_text("abc");
        s.set_text("中文");
        assert_eq!((s.text(), s.cursor()), ("中文", 6));
        s.clear();
        assert!(s.is_empty());
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn handle_key_maps_the_editing_keys_and_leaves_the_rest() {
        let mut s = InputState::with_text("one two");
        assert!(s.handle_key(ctrl('a')));
        assert_eq!(s.cursor(), 0);
        assert!(s.handle_key(ctrl('e')));
        assert_eq!(s.cursor(), 7);
        assert!(s.handle_key(alt('b')));
        assert_eq!(s.cursor(), 4);
        assert!(s.handle_key(key(KeyCode::Left, Modifiers::CTRL)));
        assert_eq!(s.cursor(), 0);
        assert!(s.handle_key(key(KeyCode::Right, Modifiers::ALT)));
        assert_eq!(s.cursor(), 3);
        assert!(s.handle_key(alt('f')));
        assert_eq!(s.cursor(), 7);
        assert!(s.handle_key(key(KeyCode::Backspace, Modifiers::NONE)));
        assert_eq!(s.text(), "one tw");
        assert!(s.handle_key(ctrl('w')));
        assert_eq!(s.text(), "one ");
        assert!(s.handle_key(key(KeyCode::Backspace, Modifiers::ALT)));
        assert_eq!(s.text(), "");
        s.set_text("abcd");
        assert!(s.handle_key(key(KeyCode::Home, Modifiers::NONE)));
        assert!(s.handle_key(key(KeyCode::Delete, Modifiers::NONE)));
        assert!(s.handle_key(ctrl('f')));
        assert!(s.handle_key(ctrl('b')));
        assert!(s.handle_key(ctrl('d')));
        assert_eq!(s.text(), "cd");
        assert!(s.handle_key(key(KeyCode::End, Modifiers::NONE)));
        assert!(s.handle_key(ctrl('h')));
        assert!(s.handle_key(ctrl('u')));
        assert!(s.is_empty());
        for other in [
            key(KeyCode::Enter, Modifiers::NONE),
            key(KeyCode::Esc, Modifiers::NONE),
            key(KeyCode::Tab, Modifiers::NONE),
            key(KeyCode::Up, Modifiers::NONE),
            key(KeyCode::F(5), Modifiers::NONE),
            ctrl('c'),
            ctrl('z'),
            alt('x'),
        ] {
            assert!(!s.handle_key(other), "{other:?}");
        }
        assert!(s.is_empty());
    }

    #[test]
    fn a_shifted_letter_is_typed_like_any_other() {
        let mut s = InputState::new();
        assert!(s.handle_key(key(KeyCode::Char('A'), Modifiers::SHIFT)));
        assert_eq!(s.text(), "A");
    }

    #[test]
    fn short_text_is_drawn_from_the_left_with_the_cursor_after_it() {
        let s = InputState::with_text("abc");
        assert_eq!(draw(Input::new(), &s, 8), "abc     ");
        assert_eq!(s.cursor_position(), Some((3, 0)));
    }

    #[test]
    fn long_text_scrolls_just_far_enough_to_keep_the_cursor_in_view() {
        let mut s = InputState::with_text("abcdefghij");
        assert_eq!(draw(Input::new(), &s, 5), "ghij ");
        assert_eq!(s.cursor_position(), Some((4, 0)));
        // Moving left inside the view doesn't scroll.
        s.move_left();
        s.move_left();
        assert_eq!(draw(Input::new(), &s, 5), "ghij ");
        assert_eq!(s.cursor_position(), Some((2, 0)));
        // Moving past the left edge does.
        for _ in 0..5 {
            s.move_left();
        }
        assert_eq!(draw(Input::new(), &s, 5), "defgh");
        assert_eq!(s.cursor_position(), Some((0, 0)));
    }

    #[test]
    fn the_cursor_stays_visible_at_every_width_and_position() {
        for w in 1..=8u16 {
            for text in [
                "abcdefghij",
                "中文字符串好",
                "a中b文c字",
                &format!("x{FAMILY}y{FAMILY}z"),
            ] {
                let mut s = InputState::with_text(text);
                for step in 0..30 {
                    if step % 2 == 0 {
                        s.move_left();
                    } else {
                        s.move_right();
                    }
                    if step == 9 {
                        s.home();
                    }
                    draw(Input::new(), &s, w);
                    let (x, y) = s.cursor_position().expect("the cursor has a place");
                    assert!(x < w && y == 0, "w={w} text={text:?} step={step} x={x}");
                }
            }
        }
    }

    #[test]
    fn scroll_comes_back_when_the_text_gets_shorter() {
        let mut s = InputState::with_text("abcdefghij");
        assert_eq!(draw(Input::new(), &s, 5), "ghij ");
        s.kill_to_start();
        s.insert_str("xy");
        assert_eq!(draw(Input::new(), &s, 5), "xy   ");
        assert_eq!(s.cursor_position(), Some((2, 0)));
    }

    #[test]
    fn a_wide_glyph_cut_by_the_left_edge_shows_as_blank() {
        // Columns: 中 0-1, 文 2-3, 字 4-5; the cursor at the end is column 6.
        let s = InputState::with_text("中文字");
        assert_eq!(draw(Input::new(), &s, 4), " 字 ");
        assert_eq!(s.cursor_position(), Some((3, 0)));
    }

    #[test]
    fn a_wide_glyph_is_not_drawn_half_at_the_right_edge() {
        let mut s = InputState::with_text("ab中");
        s.home();
        assert_eq!(draw(Input::new(), &s, 3), "ab ");
        assert_eq!(s.cursor_position(), Some((0, 0)));
    }

    #[test]
    fn the_cursor_position_follows_wide_characters() {
        let mut s = InputState::with_text("a中b");
        draw(Input::new(), &s, 10);
        assert_eq!(s.cursor_position(), Some((4, 0)));
        s.move_left();
        draw(Input::new(), &s, 10);
        assert_eq!(s.cursor_position(), Some((3, 0)));
        s.move_left();
        draw(Input::new(), &s, 10);
        assert_eq!(s.cursor_position(), Some((1, 0)));
    }

    #[test]
    fn the_placeholder_shows_only_while_empty() {
        let mut s = InputState::new();
        assert_eq!(draw(Input::new().placeholder("type here"), &s, 6), "type h");
        assert_eq!(s.cursor_position(), Some((0, 0)));
        s.insert_char('x');
        assert_eq!(draw(Input::new().placeholder("type here"), &s, 6), "x     ");
    }

    #[test]
    fn styles_apply_to_the_text_and_the_placeholder() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::new(area);
        let s = InputState::new();
        Input::new()
            .style(Style::new().bg(Color::Blue))
            .placeholder("hi")
            .render(area, &mut buf, &s);
        assert_eq!(
            buf.get(0, 0).unwrap().style(),
            Style::new().bg(Color::Blue).dim()
        );
        assert_eq!(buf.get(5, 0).unwrap().style(), Style::new().bg(Color::Blue));
        let mut buf = Buffer::new(area);
        let s = InputState::with_text("ab");
        Input::new()
            .style(Style::new().fg(Color::Red))
            .render(area, &mut buf, &s);
        assert_eq!(buf.get(1, 0).unwrap().style().fg, Some(Color::Red));
    }

    #[test]
    fn only_the_first_row_is_used_and_a_block_goes_around_it() {
        let area = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::new(area);
        let s = InputState::with_text("hey");
        Input::new()
            .block(Block::bordered())
            .render(area, &mut buf, &s);
        assert_eq!(rows_of(&buf), ["┌──────┐", "│hey   │", "└──────┘"]);
        assert_eq!(s.cursor_position(), Some((4, 1)));
        let mut buf = Buffer::new(Rect::new(0, 0, 5, 3));
        Input::new().render(Rect::new(0, 0, 5, 3), &mut buf, &s);
        assert_eq!(rows_of(&buf), ["hey  ", "     ", "     "]);
    }

    #[test]
    fn empty_areas_leave_no_cursor_and_do_not_panic() {
        let s = InputState::with_text("abc");
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 2));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 3, 0),
            Rect::new(9, 9, 3, 3),
        ] {
            Input::new().render(area, &mut buf, &s);
            assert_eq!(s.cursor_position(), None);
        }
        Input::new()
            .block(Block::bordered())
            .render(Rect::new(0, 0, 2, 2), &mut buf, &s);
        assert_eq!(s.cursor_position(), None);
        assert!(buf.cells().iter().all(|c| c.symbol() != "a"));
    }

    #[test]
    fn frame_render_input_sets_the_terminal_cursor() {
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::new(area);
        let s = InputState::with_text("hi");
        let mut frame = Frame::new(&mut buf);
        frame.render_input(Input::new(), Rect::new(2, 1, 6, 1), &s);
        assert_eq!(frame.cursor(), Some((4, 1)));
    }

    /// Any run of edits keeps the text free of control characters and the
    /// cursor inside the text on a cluster boundary.
    #[test]
    fn random_edits_keep_the_cursor_on_a_boundary() {
        let pieces = [
            "🇧", "🇷", "\u{1100}", "\u{1161}", "a", "é", "e\u{301}", "\u{301}", "中", "😀", FAMILY,
            "🇧🇷", "👍🏽", " ", "\n", "ab", "\u{200d}", "x y",
        ];
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m) as usize
        };
        let mut s = InputState::new();
        for step in 0..20_000 {
            match next(12) {
                0..=2 => s.insert_str(pieces[next(pieces.len() as u64)]),
                3 => {
                    s.backspace();
                }
                4 => {
                    s.delete();
                }
                5 => s.move_left(),
                6 => s.move_right(),
                7 => s.move_word_left(),
                8 => s.move_word_right(),
                9 => {
                    if next(4) == 0 {
                        s.kill_to_end();
                    } else {
                        s.delete_word_back();
                    }
                }
                10 => {
                    if next(20) == 0 {
                        s.kill_to_start();
                    } else {
                        s.home();
                    }
                }
                _ => s.end(),
            }
            let text = s.text();
            assert!(s.cursor() <= text.len(), "step {step}");
            assert!(text.is_char_boundary(s.cursor()), "step {step}");
            assert_eq!(
                snap(text, s.cursor()),
                s.cursor(),
                "cursor inside a cluster at step {step}: {text:?}"
            );
            assert!(!text.chars().any(char::is_control));
            draw(Input::new(), &s, 1 + next(9) as u16);
        }
    }

    #[test]
    fn deleting_between_two_halves_of_a_cluster_keeps_the_cursor_on_a_boundary() {
        // Regional indicators pair up, and so do Hangul jamo.
        for (before, mid, after) in [("🇧", "x", "🇷"), ("\u{1100}", "x", "\u{1161}")] {
            let joined = format!("{before}{after}");
            let mut s = InputState::with_text(format!("{before}{mid}{after}"));
            s.move_left();
            assert!(s.backspace());
            assert_eq!(s.text(), joined);
            assert_eq!(s.cursor(), s.text().len(), "backspace");
            s.insert_char('z');
            assert_eq!(s.text(), format!("{joined}z"));

            let mut s = InputState::with_text(format!("{before}{mid}{after}"));
            s.home();
            s.move_right();
            assert!(s.delete());
            assert_eq!(s.text(), joined);
            assert_eq!(s.cursor(), s.text().len(), "delete");

            let mut s = InputState::with_text(format!("{before}{mid}{after}"));
            s.move_left();
            assert!(s.delete_word_back());
            assert_eq!(s.text(), after);
        }
    }

    #[test]
    fn drawing_does_not_change_equality() {
        let a = InputState::with_text("abc");
        let b = InputState::with_text("abc");
        draw(Input::new(), &a, 2);
        assert_eq!(a, b);
        let mut c = InputState::with_text("abc");
        c.move_left();
        assert_ne!(a, c);
    }

    #[test]
    fn showing_the_placeholder_forgets_the_old_scroll() {
        let mut s = InputState::with_text("abcdefghijklmnop");
        draw(Input::new(), &s, 5);
        s.clear();
        draw(Input::new().placeholder("hint"), &s, 5);
        s.set_text("abcdefghi");
        for _ in 0..5 {
            s.move_left();
        }
        assert_eq!(draw(Input::new(), &s, 5), "abcde");
        assert_eq!(s.cursor_position(), Some((4, 0)));
    }

    #[test]
    fn a_wide_glyph_cut_by_the_left_edge_blanks_what_was_under_it() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::new(area);
        buf.set_string(0, 0, "xxxx", Style::new());
        let s = InputState::with_text("中文字");
        Input::new().render(area, &mut buf, &s);
        // The cell after the text is left alone, like every widget does.
        assert_eq!(rows_of(&buf), [" 字x"]);
    }
}

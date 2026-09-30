//! Terminal input as typed events.
//!
//! A [`Parser`] turns the bytes a terminal sends into [`Event`]s. It never
//! reads anything itself: the reader gives it bytes as they arrive, asks
//! for events, and calls [`Parser::escape_timeout`] when no more bytes
//! showed up in time. That last part is how a lone Esc key is told apart
//! from the start of an escape sequence, which the bytes alone can't say.

use std::ops::{BitOr, BitOrAssign};

/// Something the user or the terminal did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// A key press.
    Key(KeyEvent),
    /// A mouse action. Only sent when mouse capture is on.
    Mouse(MouseEvent),
    /// Text pasted through bracketed paste, delivered as one piece so it
    /// never looks like typing.
    Paste(String),
    /// The terminal window gained focus. Only sent when focus reports are on.
    FocusGained,
    /// The terminal window lost focus.
    FocusLost,
    /// The terminal was resized to `(columns, rows)`. Not produced by the
    /// parser; the runtime creates it from `SIGWINCH`.
    Resize(u16, u16),
}

/// A key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// Which key.
    pub code: KeyCode,
    /// Modifier keys held.
    pub modifiers: KeyModifiers,
}

impl KeyEvent {
    /// True for this key pressed on its own, with no modifier held. Shift
    /// is already in the character: `is(KeyCode::Char('Q'))` matches a
    /// capital Q.
    pub fn is(&self, code: KeyCode) -> bool {
        self.code == code && self.modifiers == KeyModifiers::NONE
    }

    /// True for Ctrl and the character `c` held together, like Ctrl+C.
    pub fn is_ctrl(&self, c: char) -> bool {
        self.code == KeyCode::Char(c) && self.modifiers.contains(KeyModifiers::CTRL)
    }
}

/// A key on the keyboard.
///
/// Letters arrive as [`KeyCode::Char`] carrying the case that was typed, so
/// `A` is `Char('A')` and the shift modifier is not set for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyCode {
    /// A character.
    Char(char),
    /// Enter or Return.
    Enter,
    /// Tab.
    Tab,
    /// Shift+Tab. Carries the shift modifier.
    BackTab,
    /// Backspace.
    Backspace,
    /// Escape.
    Esc,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Insert.
    Insert,
    /// Delete.
    Delete,
    /// A function key, `F(1)` to `F(12)`.
    F(u8),
}

/// Shift, Ctrl and Alt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyModifiers(u8);

impl KeyModifiers {
    /// No modifiers.
    pub const NONE: KeyModifiers = KeyModifiers(0);
    /// Shift.
    pub const SHIFT: KeyModifiers = KeyModifiers(1);
    /// Alt, also called Option or Meta.
    pub const ALT: KeyModifiers = KeyModifiers(1 << 1);
    /// Ctrl.
    pub const CTRL: KeyModifiers = KeyModifiers(1 << 2);

    /// True when every modifier in `other` is set.
    pub const fn contains(self, other: KeyModifiers) -> bool {
        self.0 & other.0 == other.0
    }

    /// True when no modifier is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitOr for KeyModifiers {
    type Output = KeyModifiers;
    fn bitor(self, rhs: KeyModifiers) -> KeyModifiers {
        KeyModifiers(self.0 | rhs.0)
    }
}

impl BitOrAssign for KeyModifiers {
    fn bitor_assign(&mut self, rhs: KeyModifiers) {
        self.0 |= rhs.0;
    }
}

/// A mouse action at a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MouseEvent {
    /// What happened.
    pub kind: MouseKind,
    /// Column, counted from 0.
    pub column: u16,
    /// Row, counted from 0.
    pub row: u16,
    /// Modifier keys held.
    pub modifiers: KeyModifiers,
}

/// What a mouse did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MouseKind {
    /// A button went down.
    Down(MouseButton),
    /// A button was released.
    Up(MouseButton),
    /// The mouse moved with a button held.
    Drag(MouseButton),
    /// The mouse moved with no button held.
    Moved,
    /// Wheel up.
    ScrollUp,
    /// Wheel down.
    ScrollDown,
    /// Wheel left.
    ScrollLeft,
    /// Wheel right.
    ScrollRight,
}

/// A mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MouseButton {
    /// The left button.
    Left,
    /// The middle button.
    Middle,
    /// The right button.
    Right,
}

const ESC: u8 = 0x1b;
const PASTE_END: &[u8] = b"\x1b[201~";
/// Longest CSI sequence accepted before it is treated as garbage.
const MAX_CSI: usize = 64;
/// A paste bigger than this is delivered in pieces of about this size.
const PASTE_CHUNK: usize = 1 << 20;

enum Step {
    Event(Event, usize),
    PasteStart(usize),
    Skip(usize),
    /// Drop `n` bytes and keep dropping the rest of the CSI sequence they
    /// began, which was too long to buffer.
    SkipCsi(usize),
    NeedMore,
}

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent { code, modifiers })
}

fn with_alt(step: Step, consumed_extra: usize) -> Step {
    match step {
        Step::Event(Event::Key(mut k), n) => {
            k.modifiers |= KeyModifiers::ALT;
            Step::Event(Event::Key(k), n + consumed_extra)
        }
        Step::Event(e, n) => Step::Event(e, n + consumed_extra),
        Step::Skip(n) => Step::Skip(n + consumed_extra),
        Step::SkipCsi(n) => Step::SkipCsi(n + consumed_extra),
        Step::PasteStart(n) => Step::PasteStart(n + consumed_extra),
        Step::NeedMore => Step::NeedMore,
    }
}

/// Parses the first thing in `buf`, which is not empty.
fn parse_one(buf: &[u8]) -> Step {
    if buf[0] == ESC {
        parse_escape(buf)
    } else {
        parse_plain(buf)
    }
}

fn parse_escape(buf: &[u8]) -> Step {
    match buf.get(1) {
        None => Step::NeedMore,
        Some(b'[') => parse_csi(buf),
        Some(b'O') => parse_ss3(buf),
        // Three ESCs in a row: the first is a key press of its own. Without
        // this, a long run of ESC bytes would recurse once per byte.
        Some(&ESC) if buf.get(2) == Some(&ESC) => {
            Step::Event(key(KeyCode::Esc, KeyModifiers::NONE), 1)
        }
        // ESC before anything else is Alt with that key.
        Some(_) => with_alt(parse_one(&buf[1..]), 1),
    }
}

fn parse_ss3(buf: &[u8]) -> Step {
    let Some(&c) = buf.get(2) else {
        return Step::NeedMore;
    };
    let code = match c {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        _ => return Step::Skip(3),
    };
    Step::Event(key(code, KeyModifiers::NONE), 3)
}

fn parse_csi(buf: &[u8]) -> Step {
    let mut i = 2;
    while let Some(&b) = buf.get(i) {
        match b {
            // The legacy X10 mouse report is `ESC [ M` and three raw bytes.
            b'M' if i == 2 => {
                return if buf.len() < 6 {
                    Step::NeedMore
                } else {
                    Step::Skip(6)
                };
            }
            0x40..=0x7e => return csi_step(&buf[2..i], b, i + 1),
            0x20..=0x3f if i - 2 < MAX_CSI => i += 1,
            0x20..=0x3f => return Step::SkipCsi(i),
            // A control byte or a new ESC: give up on this sequence and let
            // the next parse start at that byte.
            _ => return Step::Skip(i),
        }
    }
    Step::NeedMore
}

/// `params` are the bytes between `ESC [` and the final byte.
fn csi_step(params: &[u8], fin: u8, consumed: usize) -> Step {
    if let Some(mouse) = params.strip_prefix(b"<") {
        return match mouse_event(mouse, fin) {
            Some(e) => Step::Event(Event::Mouse(e), consumed),
            None => Step::Skip(consumed),
        };
    }
    // Private parameters and intermediates (`?`, `>`, spaces) belong to
    // replies and features that aren't key input.
    if params.iter().any(|b| !b.is_ascii_digit() && *b != b';') {
        return Step::Skip(consumed);
    }
    let nums: Vec<u32> = params
        .split(|&b| b == b';')
        .map(|p| {
            std::str::from_utf8(p)
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0)
        })
        .collect();
    let first = nums.first().copied().unwrap_or(0);
    let mods = nums
        .get(1)
        .map_or(KeyModifiers::NONE, |&m| xterm_modifiers(m));

    let code = match fin {
        b'A' => KeyCode::Up,
        b'B' => KeyCode::Down,
        b'C' => KeyCode::Right,
        b'D' => KeyCode::Left,
        b'H' => KeyCode::Home,
        b'F' => KeyCode::End,
        b'P' => KeyCode::F(1),
        b'Q' => KeyCode::F(2),
        b'R' => KeyCode::F(3),
        b'S' => KeyCode::F(4),
        b'Z' => return Step::Event(key(KeyCode::BackTab, KeyModifiers::SHIFT | mods), consumed),
        b'I' if params.is_empty() => return Step::Event(Event::FocusGained, consumed),
        b'O' if params.is_empty() => return Step::Event(Event::FocusLost, consumed),
        b'~' => match first {
            200 if nums.len() == 1 => return Step::PasteStart(consumed),
            1 | 7 => KeyCode::Home,
            2 => KeyCode::Insert,
            3 => KeyCode::Delete,
            4 | 8 => KeyCode::End,
            5 => KeyCode::PageUp,
            6 => KeyCode::PageDown,
            11..=15 => KeyCode::F((first - 10) as u8),
            17..=21 => KeyCode::F((first - 11) as u8),
            23 | 24 => KeyCode::F((first - 12) as u8),
            _ => return Step::Skip(consumed),
        },
        _ => return Step::Skip(consumed),
    };
    Step::Event(key(code, mods), consumed)
}

/// xterm sends modifiers as `1 + bitmask`: shift 1, alt 2, ctrl 4, meta 8.
fn xterm_modifiers(param: u32) -> KeyModifiers {
    let bits = param.saturating_sub(1);
    let mut m = KeyModifiers::NONE;
    if bits & 1 != 0 {
        m |= KeyModifiers::SHIFT;
    }
    if bits & (2 | 8) != 0 {
        m |= KeyModifiers::ALT;
    }
    if bits & 4 != 0 {
        m |= KeyModifiers::CTRL;
    }
    m
}

/// `params` is what follows `ESC [ <`: `button;column;row`.
fn mouse_event(params: &[u8], fin: u8) -> Option<MouseEvent> {
    if fin != b'M' && fin != b'm' {
        return None;
    }
    let text = std::str::from_utf8(params).ok()?;
    let mut parts = text.split(';').map(|p| p.parse::<u32>().ok());
    let b = parts.next()??;
    let x = parts.next()??;
    let y = parts.next()??;
    if parts.next().is_some() {
        return None;
    }
    if b & 128 != 0 {
        // Buttons 8 to 11 (back, forward) have no `MouseButton`.
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    if b & 4 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if b & 8 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if b & 16 != 0 {
        modifiers |= KeyModifiers::CTRL;
    }
    let button = match b & 3 {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    };
    let kind = if b & 64 != 0 {
        match b & 3 {
            0 => MouseKind::ScrollUp,
            1 => MouseKind::ScrollDown,
            2 => MouseKind::ScrollLeft,
            _ => MouseKind::ScrollRight,
        }
    } else if b & 32 != 0 {
        button.map_or(MouseKind::Moved, MouseKind::Drag)
    } else if fin == b'm' {
        MouseKind::Up(button?)
    } else {
        MouseKind::Down(button?)
    };
    let cell = |v: u32| v.saturating_sub(1).min(u16::MAX as u32) as u16;
    Some(MouseEvent {
        kind,
        column: cell(x),
        row: cell(y),
        modifiers,
    })
}

/// A byte that is not part of an escape sequence: a control character or
/// the start of a UTF-8 character.
fn parse_plain(buf: &[u8]) -> Step {
    let ctrl = |c: char| Step::Event(key(KeyCode::Char(c), KeyModifiers::CTRL), 1);
    let plain = |code: KeyCode| Step::Event(key(code, KeyModifiers::NONE), 1);
    match buf[0] {
        b'\r' => plain(KeyCode::Enter),
        b'\t' => plain(KeyCode::Tab),
        0x7f => plain(KeyCode::Backspace),
        0x08 => Step::Event(key(KeyCode::Backspace, KeyModifiers::CTRL), 1),
        0x00 => ctrl(' '),
        b @ 0x01..=0x1a => ctrl((b - 1 + b'a') as char),
        b @ 0x1c..=0x1f => ctrl(['\\', ']', '^', '_'][(b - 0x1c) as usize]),
        b @ 0x20..=0x7e => plain(KeyCode::Char(b as char)),
        b => {
            let len = match b {
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => return Step::Skip(1),
            };
            let have = buf.len().min(len);
            if buf[1..have].iter().any(|c| c & 0xc0 != 0x80) {
                return Step::Skip(1);
            }
            if buf.len() < len {
                return Step::NeedMore;
            }
            match std::str::from_utf8(&buf[..len]) {
                Ok(s) => Step::Event(
                    key(
                        KeyCode::Char(s.chars().next().unwrap_or('\u{fffd}')),
                        KeyModifiers::NONE,
                    ),
                    len,
                ),
                Err(_) => Step::Skip(1),
            }
        }
    }
}

struct Paste {
    data: Vec<u8>,
    scanned: usize,
}

/// Turns terminal bytes into [`Event`]s.
///
/// ```
/// use crewtui::{Event, KeyCode, Parser};
///
/// let mut parser = Parser::new();
/// parser.feed(b"a\x1b[A");
/// assert!(matches!(parser.next_event(), Some(Event::Key(k)) if k.code == KeyCode::Char('a')));
/// assert!(matches!(parser.next_event(), Some(Event::Key(k)) if k.code == KeyCode::Up));
/// assert_eq!(parser.next_event(), None);
/// ```
///
/// Bytes can arrive in any split: an escape sequence cut in two by a read
/// is completed by the next `feed`. Input that means nothing, such as an
/// unknown escape sequence or invalid UTF-8, is dropped rather than
/// blocking the bytes behind it. A paste larger than a megabyte arrives as
/// several `Paste` events, and [`Parser::abort_paste`] ends one that never
/// got its end marker.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    paste: Option<Paste>,
    /// Inside a CSI sequence that was too long to keep, dropping bytes up to
    /// its final byte.
    discarding_csi: bool,
}

impl Parser {
    /// An empty parser.
    pub fn new() -> Self {
        Parser::default()
    }

    /// Adds bytes read from the terminal.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next complete event, or `None` when more bytes are needed.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            if self.paste.is_some() {
                return self.next_paste();
            }
            if self.discarding_csi && !self.discard_csi_bytes() {
                return None;
            }
            if self.buf.is_empty() {
                return None;
            }
            match parse_one(&self.buf) {
                Step::Event(event, n) => {
                    self.buf.drain(..n);
                    return Some(event);
                }
                Step::Skip(n) => {
                    self.buf.drain(..n.max(1));
                }
                Step::SkipCsi(n) => {
                    self.buf.drain(..n);
                    self.discarding_csi = true;
                }
                Step::PasteStart(n) => {
                    self.buf.drain(..n);
                    self.paste = Some(Paste {
                        data: Vec::new(),
                        scanned: 0,
                    });
                }
                Step::NeedMore => return None,
            }
        }
    }

    /// Drops the tail of an oversized CSI sequence. Returns false when the
    /// buffer ran out before its final byte.
    fn discard_csi_bytes(&mut self) -> bool {
        let params = self
            .buf
            .iter()
            .take_while(|b| (0x20..=0x3f).contains(*b))
            .count();
        self.buf.drain(..params);
        match self.buf.first() {
            None => false,
            Some(0x40..=0x7e) => {
                self.buf.remove(0);
                self.discarding_csi = false;
                true
            }
            Some(_) => {
                self.discarding_csi = false;
                true
            }
        }
    }

    fn next_paste(&mut self) -> Option<Event> {
        let paste = self.paste.as_mut()?;
        paste.data.append(&mut self.buf);
        let from = paste.scanned.saturating_sub(PASTE_END.len());
        let end = paste.data[from..]
            .windows(PASTE_END.len())
            .position(|w| w == PASTE_END)
            .map(|p| p + from);
        let Some(end) = end else {
            if paste.data.len() < PASTE_CHUNK {
                paste.scanned = paste.data.len();
                return None;
            }
            // Hand over what has piled up, keeping the last few bytes in
            // case they are the start of the terminator, and never cutting
            // through a UTF-8 character.
            let mut cut = paste.data.len() - PASTE_END.len();
            while cut > 0 && paste.data[cut] & 0xc0 == 0x80 {
                cut -= 1;
            }
            let text = String::from_utf8_lossy(&paste.data[..cut]).into_owned();
            paste.data.drain(..cut);
            paste.scanned = 0;
            return Some(Event::Paste(text));
        };
        let text = String::from_utf8_lossy(&paste.data[..end]).into_owned();
        self.buf = paste.data.split_off(end + PASTE_END.len());
        self.paste = None;
        Some(Event::Paste(text))
    }

    /// True while inside a bracketed paste, waiting for its end marker.
    pub fn is_pasting(&self) -> bool {
        self.paste.is_some()
    }

    /// Ends a paste whose end marker never came, for a reader that saw no
    /// input for a long time while [`Parser::is_pasting`] was true. What was
    /// collected so far is returned as a `Paste`, and parsing goes back to
    /// normal. Without this, a lost end marker would swallow everything
    /// typed afterwards.
    pub fn abort_paste(&mut self) -> Option<Event> {
        let mut paste = self.paste.take()?;
        self.buf.append(&mut paste.data);
        let text = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
        (!text.is_empty()).then_some(Event::Paste(text))
    }

    /// True when bytes are waiting that may become an event once more
    /// arrive. A reader that sees this should wait a short while for input
    /// and call [`Parser::escape_timeout`] if none comes.
    pub fn is_waiting(&self) -> bool {
        self.paste.is_none() && !self.buf.is_empty()
    }

    /// Call when [`Parser::is_waiting`] is true and no more bytes arrived in
    /// time. A leading Esc that was waiting to see if a sequence followed
    /// becomes an Esc key press, and the bytes after it are parsed as
    /// ordinary input. Anything else that was left half-received is dropped.
    pub fn escape_timeout(&mut self) -> Option<Event> {
        if self.paste.is_some() || self.buf.is_empty() {
            return None;
        }
        if self.buf[0] == ESC {
            self.buf.remove(0);
            return Some(key(KeyCode::Esc, KeyModifiers::NONE));
        }
        self.buf.clear();
        None
    }
}

impl std::fmt::Debug for Parser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Parser")
            .field("buffered", &self.buf.len())
            .field("in_paste", &self.paste.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(parser: &mut Parser) -> Vec<Event> {
        std::iter::from_fn(|| parser.next_event()).collect()
    }

    fn parse(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        p.feed(bytes);
        events(&mut p)
    }

    /// Feeds one byte at a time, so every possible split point is hit.
    fn parse_split(bytes: &[u8]) -> Vec<Event> {
        let mut p = Parser::new();
        let mut out = Vec::new();
        for b in bytes {
            p.feed(&[*b]);
            out.extend(events(&mut p));
        }
        out
    }

    fn k(code: KeyCode) -> Event {
        key(code, KeyModifiers::NONE)
    }

    #[test]
    fn key_helpers_match_a_key_on_its_own_or_with_ctrl() {
        let plain = KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
        };
        let ctrl = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CTRL,
        };
        let ctrl_alt = KeyEvent {
            code: KeyCode::Char('c'),
            modifiers: KeyModifiers::CTRL | KeyModifiers::ALT,
        };
        assert!(plain.is(KeyCode::Char('q')));
        assert!(!plain.is(KeyCode::Char('w')));
        assert!(!plain.is_ctrl('q'));
        assert!(ctrl.is_ctrl('c'));
        assert!(!ctrl.is(KeyCode::Char('c')), "Ctrl is a modifier");
        assert!(!ctrl.is_ctrl('d'));
        assert!(ctrl_alt.is_ctrl('c'));
        assert!(!ctrl_alt.is(KeyCode::Char('c')));
    }

    fn km(code: KeyCode, m: KeyModifiers) -> Event {
        key(code, m)
    }

    fn ch(c: char) -> Event {
        k(KeyCode::Char(c))
    }

    const CTRL: KeyModifiers = KeyModifiers::CTRL;
    const ALT: KeyModifiers = KeyModifiers::ALT;
    const SHIFT: KeyModifiers = KeyModifiers::SHIFT;

    #[test]
    fn keys_and_sequences() {
        let cases: Vec<(&[u8], Vec<Event>)> = vec![
            (b"a", vec![ch('a')]),
            (b"A", vec![ch('A')]),
            (b"hi!", vec![ch('h'), ch('i'), ch('!')]),
            (b"\r", vec![k(KeyCode::Enter)]),
            (b"\t", vec![k(KeyCode::Tab)]),
            (b"\x7f", vec![k(KeyCode::Backspace)]),
            (b"\x08", vec![km(KeyCode::Backspace, CTRL)]),
            (b"\x01", vec![km(KeyCode::Char('a'), CTRL)]),
            (b"\x03", vec![km(KeyCode::Char('c'), CTRL)]),
            (b"\x1a", vec![km(KeyCode::Char('z'), CTRL)]),
            (b"\x00", vec![km(KeyCode::Char(' '), CTRL)]),
            (b"\x1c", vec![km(KeyCode::Char('\\'), CTRL)]),
            (b"\x1f", vec![km(KeyCode::Char('_'), CTRL)]),
            (b"\x1b[A", vec![k(KeyCode::Up)]),
            (b"\x1b[B", vec![k(KeyCode::Down)]),
            (b"\x1b[C", vec![k(KeyCode::Right)]),
            (b"\x1b[D", vec![k(KeyCode::Left)]),
            (b"\x1b[H", vec![k(KeyCode::Home)]),
            (b"\x1b[F", vec![k(KeyCode::End)]),
            (b"\x1bOA", vec![k(KeyCode::Up)]),
            (b"\x1bOH", vec![k(KeyCode::Home)]),
            (b"\x1b[Z", vec![km(KeyCode::BackTab, SHIFT)]),
            (b"\x1b[1~", vec![k(KeyCode::Home)]),
            (b"\x1b[2~", vec![k(KeyCode::Insert)]),
            (b"\x1b[3~", vec![k(KeyCode::Delete)]),
            (b"\x1b[4~", vec![k(KeyCode::End)]),
            (b"\x1b[5~", vec![k(KeyCode::PageUp)]),
            (b"\x1b[6~", vec![k(KeyCode::PageDown)]),
            (b"\x1bOP", vec![k(KeyCode::F(1))]),
            (b"\x1bOS", vec![k(KeyCode::F(4))]),
            (b"\x1b[11~", vec![k(KeyCode::F(1))]),
            (b"\x1b[15~", vec![k(KeyCode::F(5))]),
            (b"\x1b[17~", vec![k(KeyCode::F(6))]),
            (b"\x1b[21~", vec![k(KeyCode::F(10))]),
            (b"\x1b[23~", vec![k(KeyCode::F(11))]),
            (b"\x1b[24~", vec![k(KeyCode::F(12))]),
            (b"\x1b[I", vec![Event::FocusGained]),
            (b"\x1b[O", vec![Event::FocusLost]),
        ];
        for (bytes, want) in cases {
            assert_eq!(parse(bytes), want, "{bytes:?}");
            assert_eq!(parse_split(bytes), want, "split {bytes:?}");
        }
    }

    #[test]
    fn xterm_modifier_encoding() {
        let cases: Vec<(&[u8], Event)> = vec![
            (b"\x1b[1;2A", km(KeyCode::Up, SHIFT)),
            (b"\x1b[1;3A", km(KeyCode::Up, ALT)),
            (b"\x1b[1;5A", km(KeyCode::Up, CTRL)),
            (b"\x1b[1;6C", km(KeyCode::Right, CTRL | SHIFT)),
            (b"\x1b[1;7D", km(KeyCode::Left, CTRL | ALT)),
            (b"\x1b[1;8H", km(KeyCode::Home, CTRL | ALT | SHIFT)),
            (b"\x1b[1;9F", km(KeyCode::End, ALT)),
            (b"\x1b[3;5~", km(KeyCode::Delete, CTRL)),
            (b"\x1b[5;2~", km(KeyCode::PageUp, SHIFT)),
            (b"\x1b[15;5~", km(KeyCode::F(5), CTRL)),
            (b"\x1b[1;2P", km(KeyCode::F(1), SHIFT)),
            (b"\x1b[1;5S", km(KeyCode::F(4), CTRL)),
        ];
        for (bytes, want) in cases {
            assert_eq!(parse(bytes), vec![want.clone()], "{bytes:?}");
            assert_eq!(parse_split(bytes), vec![want], "split {bytes:?}");
        }
    }

    #[test]
    fn alt_is_an_escape_prefix() {
        assert_eq!(parse(b"\x1bx"), vec![km(KeyCode::Char('x'), ALT)]);
        assert_eq!(parse(b"\x1b\r"), vec![km(KeyCode::Enter, ALT)]);
        assert_eq!(parse(b"\x1b\x7f"), vec![km(KeyCode::Backspace, ALT)]);
        assert_eq!(parse(b"\x1b\x01"), vec![km(KeyCode::Char('a'), CTRL | ALT)]);
        assert_eq!(parse("\x1bé".as_bytes()), vec![km(KeyCode::Char('é'), ALT)]);
        assert_eq!(parse(b"\x1b\x1b[A"), vec![km(KeyCode::Up, ALT)]);
        assert_eq!(parse_split(b"\x1b\x1b[A"), vec![km(KeyCode::Up, ALT)]);
    }

    #[test]
    fn utf8_text_including_split_characters() {
        for text in ["é", "中", "😀", "aé中😀b"] {
            let want: Vec<Event> = text.chars().map(ch).collect();
            assert_eq!(parse(text.as_bytes()), want, "{text}");
            assert_eq!(parse_split(text.as_bytes()), want, "split {text}");
        }
    }

    #[test]
    fn mouse_sgr_events() {
        let m = |kind, column, row, modifiers| {
            Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            })
        };
        use MouseButton::*;
        use MouseKind::*;
        let cases: Vec<(&[u8], Event)> = vec![
            (b"\x1b[<0;10;5M", m(Down(Left), 9, 4, KeyModifiers::NONE)),
            (b"\x1b[<0;10;5m", m(Up(Left), 9, 4, KeyModifiers::NONE)),
            (b"\x1b[<1;1;1M", m(Down(Middle), 0, 0, KeyModifiers::NONE)),
            (b"\x1b[<2;3;4M", m(Down(Right), 2, 3, KeyModifiers::NONE)),
            (b"\x1b[<32;7;8M", m(Drag(Left), 6, 7, KeyModifiers::NONE)),
            (b"\x1b[<35;7;8M", m(Moved, 6, 7, KeyModifiers::NONE)),
            (b"\x1b[<64;2;2M", m(ScrollUp, 1, 1, KeyModifiers::NONE)),
            (b"\x1b[<65;2;2M", m(ScrollDown, 1, 1, KeyModifiers::NONE)),
            (b"\x1b[<66;2;2M", m(ScrollLeft, 1, 1, KeyModifiers::NONE)),
            (b"\x1b[<67;2;2M", m(ScrollRight, 1, 1, KeyModifiers::NONE)),
            (b"\x1b[<4;2;2M", m(Down(Left), 1, 1, SHIFT)),
            (b"\x1b[<8;2;2M", m(Down(Left), 1, 1, ALT)),
            (b"\x1b[<16;2;2M", m(Down(Left), 1, 1, CTRL)),
            (
                b"\x1b[<0;300;70000M",
                m(Down(Left), 299, 65535, KeyModifiers::NONE),
            ),
        ];
        for (bytes, want) in cases {
            assert_eq!(parse(bytes), vec![want.clone()], "{bytes:?}");
            assert_eq!(parse_split(bytes), vec![want], "split {bytes:?}");
        }
    }

    #[test]
    fn malformed_mouse_reports_are_dropped() {
        for bytes in [
            &b"\x1b[<0;1M"[..],
            b"\x1b[<;1;1M",
            b"\x1b[<0;1;1;1M",
            b"\x1b[<0;1;1X",
        ] {
            assert_eq!(parse(bytes), vec![], "{bytes:?}");
        }
        assert_eq!(parse(b"\x1b[<0;1M\x1b[A"), vec![k(KeyCode::Up)]);
        // `a` is a final byte, so this is an unknown sequence followed by text.
        assert_eq!(
            parse(b"\x1b[<a;1;1M"),
            vec![ch(';'), ch('1'), ch(';'), ch('1'), ch('M')]
        );
    }

    #[test]
    fn bracketed_paste_is_one_event_and_never_keys() {
        let want = vec![Event::Paste("hi\r\nx\x1b[Ay\x03".to_string())];
        let bytes = b"\x1b[200~hi\r\nx\x1b[Ay\x03\x1b[201~";
        assert_eq!(parse(bytes), want);
        assert_eq!(parse_split(bytes), want);
    }

    #[test]
    fn paste_boundaries_and_neighbours() {
        assert_eq!(
            parse(b"a\x1b[200~x\x1b[201~b"),
            vec![ch('a'), Event::Paste("x".into()), ch('b')]
        );
        assert_eq!(
            parse(b"\x1b[200~\x1b[201~"),
            vec![Event::Paste(String::new())]
        );
        let bytes = "\x1b[200~中é😀\x1b[201~".as_bytes();
        assert_eq!(parse_split(bytes), vec![Event::Paste("中é😀".into())]);
        // A terminator split across reads is still found.
        let mut p = Parser::new();
        p.feed(b"\x1b[200~ab\x1b[20");
        assert_eq!(p.next_event(), None);
        p.feed(b"1~c");
        assert_eq!(p.next_event(), Some(Event::Paste("ab".into())));
        assert_eq!(p.next_event(), Some(ch('c')));
    }

    #[test]
    fn invalid_utf8_in_a_paste_is_replaced() {
        assert_eq!(
            parse(b"\x1b[200~a\xffb\x1b[201~"),
            vec![Event::Paste("a\u{fffd}b".into())]
        );
    }

    #[test]
    fn a_stray_paste_end_is_ignored() {
        assert_eq!(parse(b"a\x1b[201~b"), vec![ch('a'), ch('b')]);
    }

    #[test]
    fn a_lone_escape_waits_and_the_timeout_turns_it_into_esc() {
        let mut p = Parser::new();
        p.feed(b"\x1b");
        assert_eq!(p.next_event(), None);
        assert!(p.is_waiting());
        assert_eq!(p.escape_timeout(), Some(k(KeyCode::Esc)));
        assert!(!p.is_waiting());
        assert_eq!(p.escape_timeout(), None);
    }

    #[test]
    fn a_half_received_sequence_times_out_as_esc_then_plain_keys() {
        let mut p = Parser::new();
        p.feed(b"\x1b[1;");
        assert_eq!(p.next_event(), None);
        assert_eq!(p.escape_timeout(), Some(k(KeyCode::Esc)));
        assert_eq!(events(&mut p), vec![ch('['), ch('1'), ch(';')]);
    }

    #[test]
    fn a_sequence_completed_before_the_timeout_is_not_esc() {
        let mut p = Parser::new();
        p.feed(b"\x1b");
        assert_eq!(p.next_event(), None);
        p.feed(b"[A");
        assert_eq!(p.next_event(), Some(k(KeyCode::Up)));
        assert_eq!(p.escape_timeout(), None);
    }

    #[test]
    fn a_half_received_utf8_character_is_dropped_on_timeout() {
        let mut p = Parser::new();
        p.feed(&"中".as_bytes()[..2]);
        assert_eq!(p.next_event(), None);
        assert!(p.is_waiting());
        assert_eq!(p.escape_timeout(), None);
        assert!(!p.is_waiting());
    }

    #[test]
    fn timeout_does_not_interrupt_a_paste() {
        let mut p = Parser::new();
        p.feed(b"\x1b[200~partial");
        assert_eq!(p.next_event(), None);
        assert!(!p.is_waiting());
        assert_eq!(p.escape_timeout(), None);
        p.feed(b"\x1b[201~");
        assert_eq!(p.next_event(), Some(Event::Paste("partial".into())));
    }

    #[test]
    fn unknown_sequences_are_dropped_without_eating_what_follows() {
        assert_eq!(
            parse(b"\x1b[99Za"),
            vec![km(KeyCode::BackTab, SHIFT), ch('a')]
        );
        assert_eq!(parse(b"\x1b[?1;2cx"), vec![ch('x')]);
        assert_eq!(parse(b"\x1b[200;5~x"), vec![ch('x')]);
        assert_eq!(parse(b"\x1b[27;5;9~x"), vec![ch('x')]);
        assert_eq!(parse(b"\x1b[97;5ux"), vec![ch('x')]);
        assert_eq!(parse(b"\x1bO~x"), vec![ch('x')]);
    }

    #[test]
    fn invalid_utf8_and_stray_bytes_do_not_stall_the_parser() {
        assert_eq!(parse(b"a\xffb"), vec![ch('a'), ch('b')]);
        assert_eq!(parse(b"\xc3\x28"), vec![ch('(')]);
        assert_eq!(parse(b"\xe4\xb8a"), vec![ch('a')]);
        assert_eq!(parse(b"\x80\xbf!"), vec![ch('!')]);
        assert_eq!(parse(b"\xf8x"), vec![ch('x')]);
    }

    #[test]
    fn alt_before_a_paste_start_does_not_leave_junk_in_the_paste() {
        assert_eq!(
            parse(b"\x1b\x1b[200~hi\x1b[201~"),
            vec![Event::Paste("hi".into())]
        );
        assert_eq!(
            parse_split(b"\x1b\x1b[200~hi\x1b[201~"),
            vec![Event::Paste("hi".into())]
        );
    }

    #[test]
    fn a_paste_without_an_end_marker_can_be_aborted() {
        let mut p = Parser::new();
        p.feed(b"\x1b[200~abc");
        assert_eq!(p.next_event(), None);
        assert!(p.is_pasting());
        p.feed(b"def");
        assert_eq!(p.next_event(), None);
        assert_eq!(p.abort_paste(), Some(Event::Paste("abcdef".into())));
        assert!(!p.is_pasting());
        p.feed(b"q");
        assert_eq!(p.next_event(), Some(ch('q')));
        assert_eq!(p.abort_paste(), None);
    }

    #[test]
    fn a_huge_paste_arrives_in_pieces_without_losing_or_splitting_anything() {
        let text: String = "中é😀a".repeat(300_000);
        let mut bytes = b"\x1b[200~".to_vec();
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\x1b[201~z");
        let mut p = Parser::new();
        let mut pieces = Vec::new();
        let mut rest = Vec::new();
        for chunk in bytes.chunks(4093) {
            p.feed(chunk);
            for e in events(&mut p) {
                match e {
                    Event::Paste(t) => pieces.push(t),
                    other => rest.push(other),
                }
            }
        }
        assert!(pieces.len() > 1);
        assert!(pieces.iter().all(|t| t.len() <= 2 * PASTE_CHUNK));
        assert_eq!(pieces.concat(), text);
        assert_eq!(rest, vec![ch('z')]);
    }

    #[test]
    fn extra_mouse_buttons_are_dropped_not_read_as_left() {
        assert_eq!(parse(b"\x1b[<128;5;5M"), vec![]);
        assert_eq!(parse(b"\x1b[<129;5;5M"), vec![]);
    }

    #[test]
    fn a_legacy_x10_mouse_report_is_swallowed_whole() {
        assert_eq!(parse(b"\x1b[M !!x"), vec![ch('x')]);
        assert_eq!(parse_split(b"\x1b[M !!x"), vec![ch('x')]);
        let mut p = Parser::new();
        p.feed(b"\x1b[M !");
        assert_eq!(p.next_event(), None);
    }

    #[test]
    fn an_oversized_csi_is_dropped_to_its_final_byte() {
        let mut bytes = b"\x1b[".to_vec();
        bytes.extend(std::iter::repeat_n(b'1', 200));
        bytes.extend_from_slice(b"Aq");
        assert_eq!(parse(&bytes), vec![ch('q')]);
        assert_eq!(parse_split(&bytes), vec![ch('q')]);
    }

    #[test]
    fn an_esc_inside_a_sequence_starts_a_new_one() {
        assert_eq!(parse(b"\x1b[1\x1b[A"), vec![k(KeyCode::Up)]);
    }

    #[test]
    fn garbage_never_panics_and_always_drains() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let alphabet: &[u8] = b"\x1b[O<;0123456789~ABCDMmZIu \xff\xc3\xe4\r\t\x7f\x03q200";
        for _ in 0..2000 {
            let len = (next() % 40) as usize;
            let bytes: Vec<u8> = (0..len)
                .map(|_| {
                    if next() % 5 == 0 {
                        next() as u8
                    } else {
                        alphabet[(next() % alphabet.len() as u64) as usize]
                    }
                })
                .collect();
            let mut bytes = bytes;
            if next() % 8 == 0 {
                // Now and then an oversized CSI, to exercise the discard state.
                let at = (next() as usize) % (bytes.len() + 1);
                let mut long = b"\x1b[".to_vec();
                long.extend(std::iter::repeat_n(b'1', 70));
                bytes.splice(at..at, long);
            }
            let mut whole = Parser::new();
            whole.feed(&bytes);
            let mut a = events(&mut whole);
            while whole.is_waiting() {
                a.extend(whole.escape_timeout());
                a.extend(events(&mut whole));
            }
            let mut split = Parser::new();
            let mut b = Vec::new();
            for byte in &bytes {
                split.feed(&[*byte]);
                b.extend(events(&mut split));
            }
            while split.is_waiting() {
                b.extend(split.escape_timeout());
                b.extend(events(&mut split));
            }
            assert_eq!(a, b, "{bytes:?}");
        }
    }
}

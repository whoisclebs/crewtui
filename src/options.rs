//! What to turn on in the terminal, and the escape sequences that do it.

/// Which terminal modes [`Terminal::enter`](crate::Terminal::enter) turns on. Raw mode is always
/// enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TerminalOptions {
    /// Draw on the alternate screen, so the shell's scrollback is untouched.
    pub alternate_screen: bool,
    /// Hide the cursor while the app runs.
    pub hide_cursor: bool,
    /// Report mouse presses, releases, drags and the wheel.
    pub mouse: bool,
    /// Report when the terminal gains or loses focus.
    pub focus_events: bool,
    /// Deliver pasted text as one event instead of as typed keys.
    pub bracketed_paste: bool,
    /// Ask for the kitty keyboard protocol: keys that plain terminals can't
    /// tell apart (Ctrl+I from Tab, Esc from the start of a sequence) arrive
    /// as different events, modifiers beyond Shift, Alt and Ctrl are
    /// reported, and key releases and repeats arrive as [`KeyEvent`]s with
    /// their [`KeyEventKind`]. Off by default, since a terminal that doesn't
    /// know the protocol ignores the request and keeps sending plain keys,
    /// and an app should only expect releases when it knows it has a
    /// terminal that supports them.
    ///
    /// [`KeyEvent`]: crate::KeyEvent
    /// [`KeyEventKind`]: crate::KeyEventKind
    pub keyboard_enhancement: bool,
}

impl TerminalOptions {
    /// Draws on the alternate screen, or on the normal one.
    pub fn alternate_screen(mut self, on: bool) -> Self {
        self.alternate_screen = on;
        self
    }

    /// Hides the cursor while the app runs, or leaves it showing.
    pub fn hide_cursor(mut self, on: bool) -> Self {
        self.hide_cursor = on;
        self
    }

    /// Reports mouse presses, releases, drags and the wheel, or doesn't.
    pub fn mouse(mut self, on: bool) -> Self {
        self.mouse = on;
        self
    }

    /// Reports when the terminal gains or loses focus, or doesn't.
    pub fn focus_events(mut self, on: bool) -> Self {
        self.focus_events = on;
        self
    }

    /// Delivers pasted text as one event, or as typed keys.
    pub fn bracketed_paste(mut self, on: bool) -> Self {
        self.bracketed_paste = on;
        self
    }

    /// Asks for the kitty keyboard protocol, or doesn't. See
    /// [`TerminalOptions::keyboard_enhancement`].
    pub fn keyboard_enhancement(mut self, on: bool) -> Self {
        self.keyboard_enhancement = on;
        self
    }
}

impl Default for TerminalOptions {
    fn default() -> Self {
        TerminalOptions {
            alternate_screen: true,
            hide_cursor: true,
            mouse: false,
            focus_events: false,
            bracketed_paste: true,
            keyboard_enhancement: false,
        }
    }
}

pub(crate) fn enable_sequence(o: &TerminalOptions) -> Vec<u8> {
    let mut s = String::new();
    if o.alternate_screen {
        s.push_str("\x1b[?1049h");
    }
    if o.hide_cursor {
        s.push_str("\x1b[?25l");
    }
    if o.mouse {
        s.push_str("\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    }
    if o.focus_events {
        s.push_str("\x1b[?1004h");
    }
    if o.bracketed_paste {
        s.push_str("\x1b[?2004h");
    }
    if o.keyboard_enhancement {
        // Push flags 1 and 2: disambiguate escape codes, report event types.
        s.push_str("\x1b[>3u");
    }
    s.into_bytes()
}

/// Undoes [`enable_sequence`] in reverse order. The cursor is always shown
/// and the colors reset, since the app may have changed them by itself.
pub(crate) fn disable_sequence(o: &TerminalOptions) -> Vec<u8> {
    let mut s = String::new();
    if o.keyboard_enhancement {
        // Pop what the enable pushed.
        s.push_str("\x1b[<u");
    }
    if o.bracketed_paste {
        s.push_str("\x1b[?2004l");
    }
    if o.focus_events {
        s.push_str("\x1b[?1004l");
    }
    if o.mouse {
        s.push_str("\x1b[?1006l\x1b[?1002l\x1b[?1000l");
    }
    s.push_str("\x1b[0m\x1b[?25h");
    if o.alternate_screen {
        s.push_str("\x1b[?1049l");
    }
    s.into_bytes()
}

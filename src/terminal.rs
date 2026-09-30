//! Raw mode, alternate screen and the other terminal modes, undone on drop.
//!
//! All the `unsafe` in the crate lives here, in the small wrappers at the
//! top of the file.
#![allow(unsafe_code)]

use std::io::{self, Write};
use std::os::fd::RawFd;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError, TryLockError};
use std::thread::{self, ThreadId};

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

fn is_tty(fd: RawFd) -> bool {
    // SAFETY: `isatty` only inspects the descriptor and has no memory
    // preconditions.
    unsafe { libc::isatty(fd) == 1 }
}

fn get_termios(fd: RawFd) -> io::Result<libc::termios> {
    let mut t = std::mem::MaybeUninit::<libc::termios>::uninit();
    // SAFETY: `t` is valid for writes of a `termios`; it is only read after
    // `tcgetattr` reports success, which means it filled the struct in.
    unsafe {
        if libc::tcgetattr(fd, t.as_mut_ptr()) != 0 {
            return Err(last_error());
        }
        Ok(t.assume_init())
    }
}

fn set_termios(fd: RawFd, when: libc::c_int, t: &libc::termios) -> io::Result<()> {
    // SAFETY: `t` is a valid, initialized `termios` for the whole call.
    if unsafe { libc::tcsetattr(fd, when, t) } != 0 {
        return Err(last_error());
    }
    Ok(())
}

fn make_raw(t: &mut libc::termios) {
    // SAFETY: `t` is a valid, initialized `termios` that `cfmakeraw` edits
    // in place.
    unsafe { libc::cfmakeraw(t) }
}

fn write_all_fd(fd: RawFd, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        // SAFETY: the pointer and length come from a live slice.
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = last_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

fn window_size(fd: RawFd) -> io::Result<(u16, u16)> {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `TIOCGWINSZ` writes one `winsize` through the pointer, and
    // `ws` is valid for that.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } != 0 {
        return Err(last_error());
    }
    if ws.ws_col == 0 || ws.ws_row == 0 {
        return Err(io::Error::other("the terminal reports a size of zero"));
    }
    Ok((ws.ws_col, ws.ws_row))
}

/// Which terminal modes [`Terminal::enter`] turns on. Raw mode is always
/// enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
}

impl Default for TerminalOptions {
    fn default() -> Self {
        TerminalOptions {
            alternate_screen: true,
            hide_cursor: true,
            mouse: false,
            focus_events: false,
            bracketed_paste: true,
        }
    }
}

fn enable_sequence(o: &TerminalOptions) -> Vec<u8> {
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
    s.into_bytes()
}

/// Undoes [`enable_sequence`] in reverse order. The cursor is always shown
/// and the colors reset, since the app may have changed them by itself.
fn disable_sequence(o: &TerminalOptions) -> Vec<u8> {
    let mut s = String::new();
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

struct State {
    input: RawFd,
    output: RawFd,
    original: libc::termios,
    options: TerminalOptions,
    restored: AtomicBool,
    owner: ThreadId,
}

impl State {
    /// Undoes everything `enter` did. Only the first call does any work.
    fn restore(self: &Arc<Self>) -> io::Result<()> {
        if self.restored.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let written = write_all_fd(self.output, &disable_sequence(&self.options));
        let reset = set_termios(self.input, libc::TCSADRAIN, &self.original);
        ACTIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| !Arc::ptr_eq(s, self));
        written.and(reset)
    }
}

/// Terminals currently in raw mode, so the panic hook can find them.
static ACTIVE: Mutex<Vec<Arc<State>>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();

/// Chains a panic hook that restores the terminals entered by the
/// panicking thread before the panic message is printed, so the message
/// lands on the normal screen. It does this even if the process aborts
/// instead of unwinding. Terminals belonging to other threads are left
/// alone: a panic in a worker doesn't end an app that survives it.
fn install_panic_hook() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let me = thread::current().id();
            let mine: Vec<Arc<State>> = match ACTIVE.try_lock() {
                Ok(active) => active.iter().filter(|s| s.owner == me).cloned().collect(),
                Err(TryLockError::Poisoned(p)) => p
                    .into_inner()
                    .iter()
                    .filter(|s| s.owner == me)
                    .cloned()
                    .collect(),
                Err(TryLockError::WouldBlock) => Vec::new(),
            };
            for state in mine {
                let _ = state.restore();
            }
            previous(info);
        }));
    });
}

/// A terminal in raw mode. Dropping it restores the terminal.
///
/// Raw mode is what makes key presses arrive one at a time, unechoed and
/// without signal generation. On top of it, [`TerminalOptions`] picks the
/// screen and reporting modes. Everything that was turned on is turned off
/// again, in reverse order, when the value is dropped, whether the app
/// returned normally, returned an error, or panicked with unwinding. A
/// panic hook covers the case where it aborts instead.
///
/// `std::process::exit` skips destructors. Call [`Terminal::restore`]
/// first if you need to exit that way.
///
/// The terminal is also a [`Write`] sink for output to the tty.
pub struct Terminal {
    state: Arc<State>,
}

impl Terminal {
    /// Enters raw mode on stdin and stdout, which must both be terminals.
    pub fn enter(options: TerminalOptions) -> io::Result<Terminal> {
        Terminal::enter_on(libc::STDIN_FILENO, libc::STDOUT_FILENO, options)
    }

    pub(crate) fn enter_on(
        input: RawFd,
        output: RawFd,
        options: TerminalOptions,
    ) -> io::Result<Terminal> {
        if !is_tty(output) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stdout is not a terminal",
            ));
        }
        let original = get_termios(input)?;
        let mut raw = original;
        make_raw(&mut raw);
        set_termios(input, libc::TCSAFLUSH, &raw)?;

        let state = Arc::new(State {
            input,
            output,
            original,
            options,
            restored: AtomicBool::new(false),
            owner: thread::current().id(),
        });
        ACTIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Arc::clone(&state));
        install_panic_hook();

        if let Err(e) = write_all_fd(output, &enable_sequence(&options)) {
            let _ = state.restore();
            return Err(e);
        }
        Ok(Terminal { state })
    }

    /// The terminal size as `(columns, rows)`.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        window_size(self.state.output)
    }

    /// Restores the terminal now instead of on drop. Calling it again, or
    /// dropping afterwards, does nothing.
    pub fn restore(&mut self) -> io::Result<()> {
        self.state.restore()
    }
}

impl Write for Terminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            // SAFETY: the pointer and length come from a live slice.
            let n = unsafe { libc::write(self.state.output, buf.as_ptr().cast(), buf.len()) };
            if n >= 0 {
                return Ok(n as usize);
            }
            let err = last_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.state.restore();
    }
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("options", &self.state.options)
            .field("restored", &self.state.restored.load(Ordering::SeqCst))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CStr;
    use std::os::fd::RawFd;

    /// A pseudo-terminal pair: the app side is `slave`, the test reads what
    /// the app wrote from `master`.
    struct Pty {
        master: RawFd,
        slave: RawFd,
    }

    static OPEN: Mutex<()> = Mutex::new(());

    impl Pty {
        fn open() -> Pty {
            let _guard = OPEN.lock().unwrap_or_else(PoisonError::into_inner);
            // SAFETY: plain libc calls; `ptsname` returns a pointer into a
            // static buffer, which the lock above keeps other threads from
            // overwriting until it is copied and used.
            unsafe {
                let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
                assert!(master >= 0, "posix_openpt: {}", last_error());
                assert_eq!(libc::grantpt(master), 0);
                assert_eq!(libc::unlockpt(master), 0);
                let name = CStr::from_ptr(libc::ptsname(master)).to_owned();
                let slave = libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY);
                assert!(slave >= 0, "open slave: {}", last_error());
                let flags = libc::fcntl(master, libc::F_GETFL);
                libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
                Pty { master, slave }
            }
        }

        /// Everything the app has written so far.
        fn output(&self) -> Vec<u8> {
            let mut out = Vec::new();
            loop {
                let mut pfd = libc::pollfd {
                    fd: self.master,
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: `pfd` is valid for one entry; `buf` for its length.
                unsafe {
                    if libc::poll(&mut pfd, 1, 100) <= 0 {
                        return out;
                    }
                    let mut buf = [0u8; 512];
                    let n = libc::read(self.master, buf.as_mut_ptr().cast(), buf.len());
                    if n <= 0 {
                        return out;
                    }
                    out.extend_from_slice(&buf[..n as usize]);
                }
            }
        }

        fn termios(&self) -> libc::termios {
            get_termios(self.slave).unwrap()
        }

        fn is_raw(&self) -> bool {
            self.termios().c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG) == 0
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // SAFETY: both descriptors were opened by `open` and are closed once.
            unsafe {
                libc::close(self.master);
                libc::close(self.slave);
            }
        }
    }

    fn same(a: &libc::termios, b: &libc::termios) -> bool {
        a.c_iflag == b.c_iflag
            && a.c_oflag == b.c_oflag
            && a.c_cflag == b.c_cflag
            && a.c_lflag == b.c_lflag
            && a.c_cc == b.c_cc
    }

    fn enter(pty: &Pty, options: TerminalOptions) -> Terminal {
        Terminal::enter_on(pty.slave, pty.slave, options).unwrap()
    }

    const ALL: TerminalOptions = TerminalOptions {
        alternate_screen: true,
        hide_cursor: true,
        mouse: true,
        focus_events: true,
        bracketed_paste: true,
    };
    const NONE: TerminalOptions = TerminalOptions {
        alternate_screen: false,
        hide_cursor: false,
        mouse: false,
        focus_events: false,
        bracketed_paste: false,
    };
    const ALL_ON: &[u8] =
        b"\x1b[?1049h\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?2004h";
    const ALL_OFF: &[u8] =
        b"\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

    #[test]
    fn enter_goes_raw_and_drop_puts_everything_back() {
        let pty = Pty::open();
        let original = pty.termios();
        assert!(!pty.is_raw());
        let term = enter(&pty, ALL);
        assert!(pty.is_raw());
        assert_eq!(pty.output(), ALL_ON);
        drop(term);
        assert!(same(&pty.termios(), &original));
        assert_eq!(pty.output(), ALL_OFF);
    }

    #[test]
    fn only_the_modes_that_were_enabled_are_disabled() {
        let pty = Pty::open();
        let term = enter(&pty, NONE);
        assert!(pty.output().is_empty());
        drop(term);
        assert_eq!(pty.output(), b"\x1b[0m\x1b[?25h");

        let term = enter(
            &pty,
            TerminalOptions {
                mouse: true,
                ..NONE
            },
        );
        pty.output();
        drop(term);
        let off = String::from_utf8(pty.output()).unwrap();
        assert!(off.contains("?1000l") && !off.contains("?2004l") && !off.contains("?1049l"));
    }

    #[test]
    fn explicit_restore_then_drop_restores_once() {
        let pty = Pty::open();
        let original = pty.termios();
        let mut term = enter(&pty, ALL);
        pty.output();
        term.restore().unwrap();
        assert!(same(&pty.termios(), &original));
        assert_eq!(pty.output(), ALL_OFF);
        term.restore().unwrap();
        drop(term);
        assert!(pty.output().is_empty());
    }

    fn fails_after_entering(pty: &Pty) -> io::Result<()> {
        let _term = enter(pty, ALL);
        Err::<(), _>(io::Error::other("boom"))?;
        unreachable!("the error above returns early");
    }

    #[test]
    fn an_error_return_restores_the_terminal() {
        let pty = Pty::open();
        let original = pty.termios();
        assert!(fails_after_entering(&pty).is_err());
        assert!(same(&pty.termios(), &original));
        let out = pty.output();
        assert!(
            out.ends_with(ALL_OFF),
            "{:?}",
            String::from_utf8_lossy(&out)
        );
    }

    #[test]
    fn a_panic_with_unwinding_restores_the_terminal() {
        let pty = Pty::open();
        let original = pty.termios();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _term = enter(&pty, ALL);
            panic!("boom");
        }));
        assert!(result.is_err());
        assert!(same(&pty.termios(), &original));
        assert!(pty.output().ends_with(ALL_OFF));
    }

    #[test]
    fn the_panic_hook_restores_even_when_nothing_gets_dropped() {
        // Leaking the guard stands in for `panic = "abort"`: no destructor
        // runs, so only the hook can put the terminal back.
        let pty = Pty::open();
        let original = pty.termios();
        let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            std::mem::forget(enter(&pty, ALL));
            panic!("boom");
        }));
        assert!(result.is_err());
        assert!(same(&pty.termios(), &original));
        assert!(pty.output().ends_with(ALL_OFF));
    }

    #[test]
    fn a_panic_on_another_thread_leaves_this_terminal_alone() {
        let pty = Pty::open();
        let term = enter(&pty, ALL);
        pty.output();
        let worker = thread::spawn(|| panic!("worker panic"));
        assert!(worker.join().is_err());
        assert!(pty.is_raw());
        assert!(pty.output().is_empty());
        drop(term);
        assert!(!pty.is_raw());
    }

    #[test]
    fn stdout_that_is_not_a_terminal_is_an_error_and_nothing_changes() {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe` writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pty = Pty::open();
        let original = pty.termios();
        let err = Terminal::enter_on(pty.slave, fds[1], ALL).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(same(&pty.termios(), &original));
        assert!(pty.output().is_empty());
        // SAFETY: closing the two pipe ends opened above.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn stdin_that_is_not_a_terminal_is_an_error_before_anything_is_written() {
        let mut fds = [0; 2];
        // SAFETY: `fds` has room for the two descriptors `pipe` writes.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let pty = Pty::open();
        assert!(Terminal::enter_on(fds[0], pty.slave, ALL).is_err());
        assert!(pty.output().is_empty());
        // SAFETY: closing the two pipe ends opened above.
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
    }

    #[test]
    fn size_reports_columns_then_rows() {
        let pty = Pty::open();
        let ws = libc::winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: `TIOCSWINSZ` reads one `winsize` through the pointer.
        assert_eq!(unsafe { libc::ioctl(pty.master, libc::TIOCSWINSZ, &ws) }, 0);
        let term = enter(&pty, NONE);
        assert_eq!(term.size().unwrap(), (100, 30));
    }

    #[test]
    fn writes_go_to_the_terminal() {
        let pty = Pty::open();
        let mut term = enter(&pty, NONE);
        term.write_all(b"hello").unwrap();
        assert_eq!(pty.output(), b"hello");
    }
}

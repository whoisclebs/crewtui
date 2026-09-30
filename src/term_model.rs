//! A minimal terminal model for tests: it applies the escape sequences the
//! renderer emits and exposes the resulting grid.
//!
//! It implements only what the renderer uses (cursor moves, SGR, clear
//! screen) plus the behaviors that matter for correctness: pending wrap
//! after writing the last column, and erasing the other half of a wide
//! glyph when one half is overwritten.
#![allow(unsafe_code)]

use unicode_segmentation::UnicodeSegmentation;

use crate::text::grapheme_width;
use crate::{Buffer, Cell, Color, Modifier, Rect, Style};

pub(crate) struct Screen {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    x: usize,
    y: usize,
    pending_wrap: bool,
    pen: Style,
    link: Option<std::sync::Arc<str>>,
    /// The link of each cell, kept apart from the cells like a terminal
    /// keeps it apart from the text.
    links: Vec<Option<std::sync::Arc<str>>>,
    pending: Vec<u8>,
    cursor_visible: bool,
}

impl Screen {
    pub(crate) fn new(width: u16, height: u16) -> Self {
        Screen {
            width: width as usize,
            height: height as usize,
            cells: vec![Cell::blank(); width as usize * height as usize],
            x: 0,
            y: 0,
            pending_wrap: false,
            pen: Style::new(),
            link: None,
            links: vec![None; width as usize * height as usize],
            pending: Vec::new(),
            // The terminal hides it on entering, and that is what the
            // renderer's output is checked against.
            cursor_visible: false,
        }
    }

    /// The cursor's `(column, row)` and whether it is showing.
    pub(crate) fn cursor(&self) -> ((usize, usize), bool) {
        ((self.x, self.y), self.cursor_visible)
    }

    /// The text of row `y`, one symbol per cell, continuation cells omitted.
    pub(crate) fn row(&self, y: usize) -> String {
        self.cells[y * self.width..(y + 1) * self.width]
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    pub(crate) fn to_buffer(&self) -> Buffer {
        let mut b = Buffer::new(Rect::new(0, 0, self.width as u16, self.height as u16));
        for y in 0..self.height {
            for x in 0..self.width {
                *b.get_mut(x as u16, y as u16).unwrap() = self.cells[y * self.width + x].clone();
            }
            // Runs of cells with the same link.
            let row = &self.links[y * self.width..(y + 1) * self.width];
            let mut x = 0;
            while x < self.width {
                let Some(url) = &row[x] else {
                    x += 1;
                    continue;
                };
                let end = (x..self.width)
                    .find(|&k| row[k].as_ref() != Some(url))
                    .unwrap_or(self.width);
                b.set_link(
                    Rect::new(x as u16, y as u16, (end - x) as u16, 1),
                    Some(url),
                );
                x = end;
            }
        }
        b
    }

    /// Applies bytes the way a terminal would: sequences may be split
    /// across calls, an ESC inside a CSI sequence aborts it, and invalid
    /// UTF-8 prints U+FFFD.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let data = std::mem::take(&mut self.pending);
        let mut i = 0;
        while i < data.len() {
            if data[i] == 0x1b {
                if i + 1 == data.len() {
                    break;
                }
                if data[i + 1] == b']' {
                    // An OSC sequence runs to BEL or ESC \. Any other ESC
                    // cancels it and starts a sequence of its own.
                    let mut j = i + 2;
                    let mut end = None;
                    let mut cancelled = None;
                    let mut incomplete = false;
                    while j < data.len() {
                        if data[j] == 0x07 {
                            end = Some((j, j + 1));
                            break;
                        }
                        if data[j] == 0x1b {
                            match data.get(j + 1) {
                                Some(b'\\') => end = Some((j, j + 2)),
                                Some(_) => cancelled = Some(j),
                                None => incomplete = true,
                            }
                            break;
                        }
                        j += 1;
                    }
                    if incomplete || (end.is_none() && cancelled.is_none()) {
                        break;
                    }
                    if let Some(at) = cancelled {
                        i = at;
                        continue;
                    }
                    let (stop, next) = end.expect("checked above");
                    let payload = std::str::from_utf8(&data[i + 2..stop]).unwrap_or("");
                    self.osc(payload);
                    i = next;
                    continue;
                }
                // An ESC right after an ESC starts over from the second one.
                if data[i + 1] == 0x1b {
                    i += 1;
                    continue;
                }
                if data[i + 1] != b'[' {
                    i += 2;
                    continue;
                }
                let mut j = i + 2;
                let mut done = false;
                while j < data.len() {
                    match data[j] {
                        0x1b => break,
                        0x40..=0x7e => {
                            let params = std::str::from_utf8(&data[i + 2..j]).unwrap_or("");
                            self.csi(params, data[j] as char);
                            done = true;
                            break;
                        }
                        _ => j += 1,
                    }
                }
                if done {
                    i = j + 1;
                } else if j == data.len() {
                    break;
                } else {
                    i = j;
                }
            } else {
                let end = data[i..]
                    .iter()
                    .position(|&b| b == 0x1b)
                    .map_or(data.len(), |p| i + p);
                match std::str::from_utf8(&data[i..end]) {
                    Ok(text) => {
                        self.print_text(text);
                        i = end;
                    }
                    Err(e) => {
                        let valid = i + e.valid_up_to();
                        self.print_text(std::str::from_utf8(&data[i..valid]).unwrap_or(""));
                        match e.error_len() {
                            Some(n) => {
                                self.print("\u{fffd}");
                                i = valid + n;
                            }
                            None if end == data.len() => {
                                i = valid;
                                break;
                            }
                            None => {
                                self.print("\u{fffd}");
                                i = end;
                            }
                        }
                    }
                }
            }
        }
        self.pending = data[i..].to_vec();
    }

    fn print_text(&mut self, text: &str) {
        for g in text.graphemes(true) {
            self.print(g);
        }
    }

    fn osc(&mut self, payload: &str) {
        let mut parts = payload.splitn(3, ';');
        match (parts.next(), parts.next(), parts.next()) {
            // Hyperlinks: OSC 8 ; params ; URL.
            (Some("8"), Some(_params), Some(url)) => {
                self.link = (!url.is_empty()).then(|| std::sync::Arc::from(url));
            }
            _ => panic!("unmodeled OSC {payload:?}"),
        }
    }

    fn csi(&mut self, params: &str, fin: char) {
        let nums: Vec<usize> = params.split(';').map(|p| p.parse().unwrap_or(0)).collect();
        match fin {
            'H' => {
                self.y = nums[0].max(1).saturating_sub(1).min(self.height - 1);
                self.x = nums
                    .get(1)
                    .copied()
                    .unwrap_or(1)
                    .max(1)
                    .saturating_sub(1)
                    .min(self.width - 1);
                self.pending_wrap = false;
            }
            'C' => {
                let n = nums[0].max(1);
                self.x = (self.x + n).min(self.width - 1);
                self.pending_wrap = false;
            }
            'J' => {
                assert_eq!(nums[0], 2, "only ED 2 is modeled");
                self.cells.iter_mut().for_each(Cell::reset);
                self.links.iter_mut().for_each(|l| *l = None);
            }
            'm' => self.sgr(&nums),
            // Private modes such as alternate screen; not modeled.
            'h' | 'l' => {
                if params == "?25" {
                    self.cursor_visible = fin == 'h';
                }
            }
            other => panic!("unmodeled CSI final {other:?}"),
        }
    }

    fn sgr(&mut self, nums: &[usize]) {
        let mut i = 0;
        while i < nums.len() {
            let n = nums[i];
            let color = |base: usize, i: &mut usize| -> Color {
                match nums[*i + 1] {
                    5 => {
                        *i += 2;
                        Color::Indexed(nums[*i] as u8)
                    }
                    2 => {
                        let c =
                            Color::Rgb(nums[*i + 2] as u8, nums[*i + 3] as u8, nums[*i + 4] as u8);
                        *i += 4;
                        let _ = base;
                        c
                    }
                    other => panic!("bad extended color {other}"),
                }
            };
            match n {
                0 => self.pen = Style::new(),
                1 => self.pen = self.pen.add_modifier(Modifier::BOLD),
                2 => self.pen = self.pen.add_modifier(Modifier::DIM),
                3 => self.pen = self.pen.add_modifier(Modifier::ITALIC),
                4 => self.pen = self.pen.add_modifier(Modifier::UNDERLINE),
                7 => self.pen = self.pen.add_modifier(Modifier::REVERSE),
                9 => self.pen = self.pen.add_modifier(Modifier::STRIKETHROUGH),
                30..=37 => self.pen.fg = Some(ansi(n - 30)),
                90..=97 => self.pen.fg = Some(ansi(n - 90 + 8)),
                40..=47 => self.pen.bg = Some(ansi(n - 40)),
                100..=107 => self.pen.bg = Some(ansi(n - 100 + 8)),
                39 => self.pen.fg = Some(Color::Default),
                49 => self.pen.bg = Some(Color::Default),
                38 => self.pen.fg = Some(color(38, &mut i)),
                48 => self.pen.bg = Some(color(48, &mut i)),
                other => panic!("unmodeled SGR {other}"),
            }
            i += 1;
        }
    }

    fn print(&mut self, g: &str) {
        let w = grapheme_width(g);
        if w == 0 {
            return;
        }
        if self.pending_wrap {
            self.x = 0;
            self.y = (self.y + 1).min(self.height - 1);
            self.pending_wrap = false;
        }
        if self.x + w > self.width {
            self.x = 0;
            self.y = (self.y + 1).min(self.height - 1);
        }
        let row = self.y * self.width;
        for k in 0..w {
            let x = self.x + k;
            if self.cells[row + x].is_continuation() && x > 0 {
                self.cells[row + x - 1].set_symbol(" ");
            } else if x + 1 < self.width && self.cells[row + x + 1].is_continuation() {
                self.cells[row + x + 1].set_symbol(" ");
            }
        }
        let mut head = Cell::blank();
        head.set_symbol(g).set_style(self.pen);
        self.cells[row + self.x] = head;
        self.links[row + self.x] = self.link.clone();
        if w == 2 {
            let mut tail = Cell::blank();
            tail.set_symbol("").set_style(self.pen);
            self.cells[row + self.x + 1] = tail;
            self.links[row + self.x + 1] = self.link.clone();
        }
        if self.x + w >= self.width {
            self.x = self.width - 1;
            self.pending_wrap = true;
        } else {
            self.x += w;
        }
    }
}

fn ansi(n: usize) -> Color {
    [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::White,
        Color::BrightBlack,
        Color::BrightRed,
        Color::BrightGreen,
        Color::BrightYellow,
        Color::BrightBlue,
        Color::BrightMagenta,
        Color::BrightCyan,
        Color::BrightWhite,
    ][n]
}

#[cfg(unix)]
pub(crate) use pty::*;

/// Pseudo-terminals and child processes, which only exist on Unix.
#[cfg(unix)]
mod pty {
    use std::ffi::CStr;
    use std::io;
    use std::os::fd::{FromRawFd, RawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::{Mutex, PoisonError};
    use std::time::{Duration, Instant};

    use crate::terminal::{get_termios, last_error};

    /// A pseudo-terminal pair: the app side is `slave`, the test reads what
    /// the app wrote from `master`.
    pub(crate) struct Pty {
        pub(crate) master: RawFd,
        pub(crate) slave: RawFd,
    }

    static OPEN: Mutex<()> = Mutex::new(());

    impl Pty {
        pub(crate) fn open() -> Pty {
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
                // Children must not inherit these: a master kept open in the
                // child would stop a hangup from ever happening.
                libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
                libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
                Pty { master, slave }
            }
        }

        /// Everything the app has written so far.
        pub(crate) fn output(&self) -> Vec<u8> {
            drain_fd(self.master, 100)
        }

        /// What the app has written, but for at most `limit` even if it keeps
        /// writing.
        pub(crate) fn output_within(&self, limit: Duration) -> Vec<u8> {
            drain_fd_until(self.master, 100, Some(Instant::now() + limit))
        }

        /// Reads output into `seen` until it contains `needle`. Returns false if
        /// `limit` passes first.
        pub(crate) fn read_until(
            &self,
            needle: &[u8],
            limit: Duration,
            seen: &mut Vec<u8>,
        ) -> bool {
            let deadline = Instant::now() + limit;
            while !seen.windows(needle.len()).any(|w| w == needle) {
                if Instant::now() >= deadline {
                    return false;
                }
                seen.extend(drain_fd(self.master, 50));
            }
            true
        }

        /// Sets the size the terminal reports.
        pub(crate) fn set_size(&self, columns: u16, rows: u16) {
            let ws = libc::winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            };
            // SAFETY: `TIOCSWINSZ` reads one `winsize` through the pointer.
            assert_eq!(
                unsafe { libc::ioctl(self.master, libc::TIOCSWINSZ, &ws) },
                0
            );
        }

        pub(crate) fn termios(&self) -> libc::termios {
            get_termios(self.slave).unwrap()
        }

        pub(crate) fn is_raw(&self) -> bool {
            self.termios().c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG) == 0
        }
    }

    impl Pty {
        /// Closes the master side, which hangs up the terminal: processes that
        /// have it as their controlling terminal get SIGHUP.
        pub(crate) fn close_master(&mut self) {
            // SAFETY: closes the descriptor once; `Drop` skips it afterwards.
            unsafe { libc::close(self.master) };
            self.master = -1;
        }

        /// Starts `cmd` with this terminal as its stdin, stdout, stderr and
        /// controlling terminal, in a session of its own. That last part is what
        /// makes hangups and terminal-generated signals reach it.
        pub(crate) fn spawn(&self, cmd: &mut Command) -> io::Result<Child> {
            let dup = || -> io::Result<Stdio> {
                // SAFETY: `dup` returns a new descriptor that `Stdio` then owns.
                let fd = unsafe { libc::dup(self.slave) };
                if fd < 0 {
                    return Err(last_error());
                }
                Ok(unsafe { Stdio::from_raw_fd(fd) })
            };
            cmd.stdin(dup()?).stdout(dup()?).stderr(dup()?);
            // SAFETY: runs between fork and exec in the child and only calls
            // async-signal-safe functions (`setsid`, `ioctl`).
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(last_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(last_error());
                    }
                    Ok(())
                });
            }
            cmd.spawn()
        }

        /// Reruns this test executable as a child on this terminal, running
        /// only `test` (a full path such as `pty_children::child_entry`) with
        /// `mode` in the environment. The test decides what to do from the
        /// mode; see `crate::pty_children`.
        pub(crate) fn spawn_self(&self, test: &str, mode: &str) -> io::Result<Child> {
            let exe = std::env::current_exe()?;
            self.spawn(
                Command::new(exe)
                    .args(["--exact", test, "--nocapture", "--test-threads=1"])
                    .env(CHILD_MODE, mode),
            )
        }
    }

    /// Sends `sig` to `child`.
    pub(crate) fn kill(child: &Child, sig: libc::c_int) {
        // SAFETY: `kill` takes a pid and a signal number.
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, sig) }, 0);
    }

    /// Waits until `child` has been stopped by a signal, and returns which one,
    /// or `None` if that didn't happen within `limit`.
    pub(crate) fn wait_until_stopped(child: &Child, limit: Duration) -> Option<libc::c_int> {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            let mut status = 0;
            // SAFETY: `status` is valid for a write, and WNOHANG returns at once.
            let got = unsafe {
                libc::waitpid(
                    child.id() as libc::pid_t,
                    &mut status,
                    libc::WUNTRACED | libc::WNOHANG,
                )
            };
            if got > 0 && libc::WIFSTOPPED(status) {
                return Some(libc::WSTOPSIG(status));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    /// The environment variable that tells a re-run test executable which
    /// scenario to play out.
    pub(crate) const CHILD_MODE: &str = "CREWTUI_PTY_CHILD";

    /// Waits for `child` to exit, up to `limit`. `None` means it is still running.
    pub(crate) fn wait_timeout(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                _ => return None,
            }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            // SAFETY: both descriptors were opened by `open` and are closed once.
            unsafe {
                if self.master >= 0 {
                    libc::close(self.master);
                }
                libc::close(self.slave);
            }
        }
    }

    pub(crate) fn same(a: &libc::termios, b: &libc::termios) -> bool {
        a.c_iflag == b.c_iflag
            && a.c_oflag == b.c_oflag
            && a.c_cflag == b.c_cflag
            && a.c_lflag == b.c_lflag
            && a.c_cc == b.c_cc
    }

    /// Reads what is available on `fd`, waiting up to `idle_ms` for more.
    pub(crate) fn drain_fd(fd: RawFd, idle_ms: libc::c_int) -> Vec<u8> {
        drain_fd_until(fd, idle_ms, None)
    }

    /// Like `drain_fd`, but also returns once `deadline` has passed, so a child
    /// that never stops writing can't keep the caller here forever.
    pub(crate) fn drain_fd_until(
        fd: RawFd,
        idle_ms: libc::c_int,
        deadline: Option<Instant>,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return out;
            }
            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` is valid for one entry; `buf` for its length.
            unsafe {
                if libc::poll(&mut pfd, 1, idle_ms) <= 0 {
                    return out;
                }
                let mut buf = [0u8; 512];
                let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
                if n <= 0 {
                    return out;
                }
                out.extend_from_slice(&buf[..n as usize]);
            }
        }
    }

    /// Writes all of `bytes` to `fd`, which must be a pty master.
    pub(crate) fn write_fd(fd: RawFd, bytes: &[u8]) {
        // SAFETY: the pointer and length come from a live slice.
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(n, bytes.len() as isize, "short write to the pty");
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;
    use std::time::Duration;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn a_child_that_leaves_the_terminal_alone_leaves_termios_unchanged() {
        let pty = Pty::open();
        let original = pty.termios();
        let mut child = pty.spawn(&mut sh("true")).unwrap();
        let status = wait_timeout(&mut child, Duration::from_secs(10)).expect("still running");
        assert!(status.success());
        assert!(same(&pty.termios(), &original));
    }

    #[test]
    fn a_child_that_leaves_the_terminal_raw_is_caught() {
        // The failure the safety tests exist to catch, produced on purpose.
        let pty = Pty::open();
        let original = pty.termios();
        let mut child = pty.spawn(&mut sh("stty raw -echo")).unwrap();
        wait_timeout(&mut child, Duration::from_secs(10)).expect("still running");
        assert!(!same(&pty.termios(), &original));
        assert!(pty.is_raw());
    }

    #[test]
    fn bytes_typed_at_the_master_reach_the_child_and_its_output_comes_back() {
        let pty = Pty::open();
        let mut child = pty.spawn(&mut sh("read x; echo got:$x")).unwrap();
        write_fd(pty.master, b"hi\n");
        wait_timeout(&mut child, Duration::from_secs(10)).expect("still running");
        let out = String::from_utf8_lossy(&pty.output()).into_owned();
        assert!(out.contains("got:hi"), "{out:?}");
    }

    #[test]
    fn an_esc_in_a_cut_sequence_starts_the_next_one_instead_of_printing_it() {
        let mut screen = Screen::new(6, 1);
        // An OSC 8 cut after its URL began, then a CSI, then text.
        screen.feed(b"\x1b]8;;http://x");
        screen.feed(b"\x1b[1mabc");
        assert_eq!(screen.row(0).trim_end(), "abc");
        // A CSI cut right after its ESC, then another.
        let mut screen = Screen::new(6, 1);
        screen.feed(b"\x1b");
        screen.feed(b"\x1b[0mxy");
        assert_eq!(screen.row(0).trim_end(), "xy");
    }

    #[test]
    fn output_from_a_child_can_be_replayed_into_the_screen_model() {
        let pty = Pty::open();
        pty.set_size(20, 3);
        let mut child = pty
            .spawn(&mut sh(r"printf '\033[2J\033[1;1Hhello \033[2;3Hworld'"))
            .unwrap();
        wait_timeout(&mut child, Duration::from_secs(10)).expect("still running");
        let mut screen = Screen::new(20, 3);
        screen.feed(&pty.output());
        assert_eq!(screen.row(0).trim_end(), "hello");
        assert_eq!(screen.row(1).trim_end(), "  world");
    }

    #[test]
    fn closing_the_master_hangs_the_child_up() {
        let mut pty = Pty::open();
        let mut child = pty.spawn(&mut sh("sleep 30")).unwrap();
        // Let it start, so the signal isn't sent to a process still forking.
        assert!(wait_timeout(&mut child, Duration::from_millis(100)).is_none());
        pty.close_master();
        let status = wait_timeout(&mut child, Duration::from_secs(10))
            .expect("the child ignored the hangup");
        assert_eq!(status.signal(), Some(libc::SIGHUP));
    }

    #[test]
    fn waiting_for_a_child_that_keeps_running_times_out() {
        let pty = Pty::open();
        let mut child = pty.spawn(&mut sh("sleep 30")).unwrap();
        assert!(wait_timeout(&mut child, Duration::from_millis(100)).is_none());
        child.kill().unwrap();
        assert!(wait_timeout(&mut child, Duration::from_secs(10)).is_some());
    }

    #[test]
    fn this_executable_can_be_rerun_as_a_child_on_a_terminal() {
        let pty = Pty::open();
        let mut child = pty
            .spawn_self("pty_children::child_entry", "hello")
            .unwrap();
        let status =
            wait_timeout(&mut child, Duration::from_secs(30)).expect("the child never finished");
        assert!(status.success(), "{status:?}");
        let out = String::from_utf8_lossy(&pty.output()).into_owned();
        assert!(out.contains("hello from the child"), "{out:?}");
    }
}

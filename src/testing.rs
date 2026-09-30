//! A minimal terminal model for tests: it applies the escape sequences the
//! renderer emits and exposes the resulting grid.
//!
//! It implements only what the renderer uses (cursor moves, SGR, clear
//! screen) plus the behaviors that matter for correctness: pending wrap
//! after writing the last column, and erasing the other half of a wide
//! glyph when one half is overwritten.
#![allow(unsafe_code)]

use std::ffi::CStr;
use std::os::fd::RawFd;
use std::sync::{Mutex, PoisonError};

use unicode_segmentation::UnicodeSegmentation;

use crate::terminal::{get_termios, last_error};
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
    pending: Vec<u8>,
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
            pending: Vec::new(),
        }
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
            }
            'm' => self.sgr(&nums),
            // Private modes such as alternate screen; not modeled.
            'h' | 'l' => {}
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
        if w == 2 {
            let mut tail = Cell::blank();
            tail.set_symbol("").set_style(self.pen);
            self.cells[row + self.x + 1] = tail;
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
            Pty { master, slave }
        }
    }

    /// Everything the app has written so far.
    pub(crate) fn output(&self) -> Vec<u8> {
        drain_fd(self.master, 100)
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

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: both descriptors were opened by `open` and are closed once.
        unsafe {
            libc::close(self.master);
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
    let mut out = Vec::new();
    loop {
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

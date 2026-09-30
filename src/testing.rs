//! A minimal terminal model for tests: it applies the escape sequences the
//! renderer emits and exposes the resulting grid.
//!
//! It implements only what the renderer uses (cursor moves, SGR, clear
//! screen) plus the behaviors that matter for correctness: pending wrap
//! after writing the last column, and erasing the other half of a wide
//! glyph when one half is overwritten.

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

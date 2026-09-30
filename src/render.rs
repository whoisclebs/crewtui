//! Turns consecutive buffers into terminal output.

use std::io::{self, Write};

use crate::{Buffer, Color, Frame, Modifier, Rect, Style};

/// Keeps the previous frame and produces the bytes that move the terminal
/// from it to the next one.
///
/// `Renderer` never touches the terminal. [`Renderer::draw`] returns the
/// escape sequences for one frame as a single slice, which the caller
/// writes with one `write_all`. An unchanged frame produces no bytes.
///
/// The previous buffer is only a belief about what the terminal shows. The
/// first frame, and the first frame after a resize, start with a clear
/// screen instead of trusting it.
#[derive(Debug)]
pub struct Renderer {
    previous: Buffer,
    current: Buffer,
    known: bool,
    out: Vec<u8>,
    /// Where the cursor was last placed, and whether it is showing.
    cursor: Option<(u16, u16)>,
    cursor_shown: bool,
}

impl Renderer {
    /// A renderer for a terminal of `width` by `height` cells.
    pub fn new(width: u16, height: u16) -> Self {
        let area = Rect::new(0, 0, width, height);
        Renderer {
            previous: Buffer::new(area),
            current: Buffer::new(area),
            known: false,
            out: Vec::new(),
            cursor: None,
            cursor_shown: false,
        }
    }

    /// The size the renderer currently draws at.
    pub fn area(&self) -> Rect {
        self.current.area()
    }

    /// Changes the drawing size. The next frame repaints everything.
    pub fn resize(&mut self, width: u16, height: u16) {
        let area = Rect::new(0, 0, width, height);
        self.previous.resize(area);
        self.current.resize(area);
        self.known = false;
    }

    /// Draws one frame. `f` receives a blank buffer covering the whole
    /// screen and must not resize it. The returned bytes are what to write
    /// to the terminal.
    ///
    /// If the caller can't write them, or only some of them get through, it
    /// must call [`Renderer::invalidate`]. [`Renderer::present`] does this.
    pub fn draw(&mut self, f: impl FnOnce(&mut Buffer)) -> &[u8] {
        self.draw_frame(|frame| f(frame.buffer_mut()))
    }

    /// Like [`Renderer::draw`], but `f` gets a [`Frame`], so it can also ask
    /// for the terminal cursor with [`Frame::set_cursor`]. The cursor is
    /// shown at that cell after the frame is drawn, and hidden when nothing
    /// asks for it. A frame that changes nothing and leaves the cursor where
    /// it was writes no bytes.
    pub fn draw_frame(&mut self, f: impl FnOnce(&mut Frame<'_>)) -> &[u8] {
        let area = self.current.area();
        self.current.reset();
        let mut frame = Frame::new(&mut self.current);
        f(&mut frame);
        let cursor = frame.cursor();
        if self.current.area() != area {
            // The frame was drawn on a buffer of another size; there is no
            // meaningful way to diff it, so show a blank one.
            self.current.resize(area);
        }
        self.out.clear();
        if !self.known {
            self.out.extend_from_slice(b"\x1b[0m\x1b[2J");
            self.previous.reset();
            self.known = true;
        }
        diff(&self.previous, &self.current, &mut self.out);
        std::mem::swap(&mut self.previous, &mut self.current);
        self.place_cursor(cursor.filter(|&(x, y)| area.contains(x, y)));
        &self.out
    }

    /// Emits what it takes to put the cursor where the frame wants it. Cells
    /// written this frame moved the terminal's cursor, so it is placed again
    /// whenever anything was written.
    fn place_cursor(&mut self, want: Option<(u16, u16)>) {
        let wrote = !self.out.is_empty();
        match want {
            Some((x, y)) => {
                if wrote || self.cursor != want || !self.cursor_shown {
                    let _ = write!(self.out, "\x1b[{};{}H", y + 1, x + 1);
                }
                if !self.cursor_shown {
                    self.out.extend_from_slice(b"\x1b[?25h");
                    self.cursor_shown = true;
                }
            }
            None => {
                if self.cursor_shown {
                    self.out.extend_from_slice(b"\x1b[?25l");
                    self.cursor_shown = false;
                }
            }
        }
        self.cursor = want;
    }

    /// Forgets whether the terminal's cursor is showing, after something
    /// reset the terminal modes behind the renderer's back.
    pub(crate) fn cursor_was_reset(&mut self) {
        self.cursor_shown = false;
        self.cursor = None;
    }

    /// Draws one frame and writes it to `out` in a single `write_all`,
    /// flushing afterwards. Nothing is written for an unchanged frame.
    ///
    /// When the write fails, possibly after part of the frame reached the
    /// terminal, the renderer no longer knows what is on screen, so it
    /// invalidates itself and the next frame repaints everything.
    pub fn present<W: Write>(
        &mut self,
        out: &mut W,
        f: impl FnOnce(&mut Buffer),
    ) -> io::Result<()> {
        self.present_frame(out, |frame| f(frame.buffer_mut()))
    }

    /// Like [`Renderer::present`], for a view that gets a [`Frame`].
    pub fn present_frame<W: Write>(
        &mut self,
        out: &mut W,
        f: impl FnOnce(&mut Frame<'_>),
    ) -> io::Result<()> {
        if self.draw_frame(f).is_empty() {
            return Ok(());
        }
        let result = out.write_all(&self.out).and_then(|()| out.flush());
        if result.is_err() {
            self.invalidate();
        }
        result
    }

    /// Forgets what the terminal shows. The next frame clears the screen
    /// and paints every cell.
    ///
    /// Call this whenever something other than this renderer may have
    /// changed the screen: another process writing to the tty, a terminal
    /// multiplexer redraw, resuming after suspend, or running a child
    /// process that took over the terminal. Resizing does it implicitly.
    pub fn invalidate(&mut self) {
        self.known = false;
    }
}

/// Appends the output that turns a terminal showing `previous` into one
/// showing `current`. Both buffers must cover the same area at the origin.
fn diff(previous: &Buffer, current: &Buffer, out: &mut Vec<u8>) {
    let area = current.area();
    let width = area.width as usize;
    let mut cursor: Option<(usize, usize)> = None;
    let mut style = Style::new();

    for y in 0..area.height as usize {
        let row = y * width;
        for x in 0..width {
            let cell = &current.cells()[row + x];
            if cell.is_continuation() {
                continue;
            }
            let wide = x + 1 < width && current.cells()[row + x + 1].is_continuation();
            let cols = if wide { 2 } else { 1 };
            // A wide glyph is drawn from its left half, so a change to
            // either half means drawing the whole glyph again.
            let dirty =
                (0..cols).any(|k| current.cells()[row + x + k] != previous.cells()[row + x + k]);
            if !dirty {
                continue;
            }
            move_cursor(out, &mut cursor, x, y);
            if cell.style() != style {
                write_style(out, cell.style());
                style = cell.style();
            }
            out.extend_from_slice(cell.symbol().as_bytes());
            // A write in the last column leaves the cursor pending a wrap
            // whose position terminals disagree about, so treat it as lost.
            cursor = (x + cols < width).then_some((x + cols, y));
        }
    }
    if style != Style::new() {
        out.extend_from_slice(b"\x1b[0m");
    }
}

fn move_cursor(out: &mut Vec<u8>, cursor: &mut Option<(usize, usize)>, x: usize, y: usize) {
    match *cursor {
        Some((cx, cy)) if cx == x && cy == y => {}
        Some((cx, cy)) if cy == y && x > cx => {
            let n = x - cx;
            if n == 1 {
                out.extend_from_slice(b"\x1b[C");
            } else {
                let _ = write!(out, "\x1b[{n}C");
            }
        }
        _ => {
            let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
        }
    }
}

fn write_style(out: &mut Vec<u8>, style: Style) {
    out.extend_from_slice(b"\x1b[0");
    let flags = [
        (Modifier::BOLD, 1),
        (Modifier::DIM, 2),
        (Modifier::ITALIC, 3),
        (Modifier::UNDERLINE, 4),
        (Modifier::REVERSE, 7),
        (Modifier::STRIKETHROUGH, 9),
    ];
    for (modifier, code) in flags {
        if style.modifiers.contains(modifier) {
            let _ = write!(out, ";{code}");
        }
    }
    if let Some(color) = style.fg {
        write_color(out, color, 30);
    }
    if let Some(color) = style.bg {
        write_color(out, color, 40);
    }
    out.push(b'm');
}

/// `base` is 30 for foreground and 40 for background.
fn write_color(out: &mut Vec<u8>, color: Color, base: u8) {
    let ansi = |n: u8| n as u16 + base as u16;
    let _ = match color {
        Color::Default => write!(out, ";{}", base + 9),
        Color::Black => write!(out, ";{}", ansi(0)),
        Color::Red => write!(out, ";{}", ansi(1)),
        Color::Green => write!(out, ";{}", ansi(2)),
        Color::Yellow => write!(out, ";{}", ansi(3)),
        Color::Blue => write!(out, ";{}", ansi(4)),
        Color::Magenta => write!(out, ";{}", ansi(5)),
        Color::Cyan => write!(out, ";{}", ansi(6)),
        Color::White => write!(out, ";{}", ansi(7)),
        Color::BrightBlack => write!(out, ";{}", ansi(0) + 60),
        Color::BrightRed => write!(out, ";{}", ansi(1) + 60),
        Color::BrightGreen => write!(out, ";{}", ansi(2) + 60),
        Color::BrightYellow => write!(out, ";{}", ansi(3) + 60),
        Color::BrightBlue => write!(out, ";{}", ansi(4) + 60),
        Color::BrightMagenta => write!(out, ";{}", ansi(5) + 60),
        Color::BrightCyan => write!(out, ";{}", ansi(6) + 60),
        Color::BrightWhite => write!(out, ";{}", ansi(7) + 60),
        Color::Indexed(n) => write!(out, ";{};5;{n}", base + 8),
        Color::Rgb(r, g, b) => write!(out, ";{};2;{r};{g};{b}", base + 8),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Screen;

    fn text(r: &mut Renderer, rows: &[&str]) -> Vec<u8> {
        r.draw(|b| {
            for (y, row) in rows.iter().enumerate() {
                b.set_string(0, y as u16, row, Style::new());
            }
        })
        .to_vec()
    }

    #[test]
    fn first_frame_clears_then_paints_only_non_blank_cells() {
        let mut r = Renderer::new(5, 2);
        assert_eq!(text(&mut r, &["hi", ""]), b"\x1b[0m\x1b[2J\x1b[1;1Hhi");
    }

    #[test]
    fn blank_first_frame_is_just_the_clear() {
        let mut r = Renderer::new(5, 2);
        assert_eq!(text(&mut r, &[]), b"\x1b[0m\x1b[2J");
    }

    #[test]
    fn unchanged_frame_produces_zero_bytes() {
        let mut r = Renderer::new(8, 2);
        text(&mut r, &["hello", "world"]);
        assert!(text(&mut r, &["hello", "world"]).is_empty());
        assert!(text(&mut r, &["hello", "world"]).is_empty());
    }

    #[test]
    fn one_changed_cell_is_one_move_and_one_symbol() {
        let mut r = Renderer::new(8, 2);
        text(&mut r, &["abc"]);
        assert_eq!(text(&mut r, &["axc"]), b"\x1b[1;2Hx");
    }

    #[test]
    fn a_run_needs_a_single_cursor_move() {
        let mut r = Renderer::new(10, 2);
        text(&mut r, &["abcdefgh"]);
        assert_eq!(text(&mut r, &["abXYZfgh"]), b"\x1b[1;3HXYZ");
    }

    #[test]
    fn nearby_changes_on_one_row_use_forward_moves() {
        let mut r = Renderer::new(10, 1);
        text(&mut r, &["abcdefgh"]);
        assert_eq!(text(&mut r, &["Xbcdefgh"]), b"\x1b[1;1HX");
        assert_eq!(text(&mut r, &["XbYdeZgh"]), b"\x1b[1;3HY\x1b[2CZ");
        assert_eq!(text(&mut r, &["XbYdeZgh"]), b"");
    }

    #[test]
    fn style_change_emits_sgr_once_per_run_and_resets_at_the_end() {
        let mut r = Renderer::new(8, 1);
        text(&mut r, &["abc"]);
        let out = r
            .draw(|b| {
                b.set_string(0, 0, "abc", Style::new().fg(Color::Red).bold());
            })
            .to_vec();
        assert_eq!(out, b"\x1b[1;1H\x1b[0;1;31mabc\x1b[0m");
    }

    #[test]
    fn style_only_change_rewrites_the_symbol() {
        let mut r = Renderer::new(8, 1);
        text(&mut r, &["a"]);
        let out = r.draw(|b| {
            b.set_string(0, 0, "a", Style::new().bg(Color::Rgb(1, 2, 3)));
        });
        assert_eq!(out, b"\x1b[1;1H\x1b[0;48;2;1;2;3ma\x1b[0m");
    }

    #[test]
    fn wide_glyph_is_emitted_once_and_advances_two_columns() {
        let mut r = Renderer::new(8, 1);
        assert_eq!(
            text(&mut r, &["a中b"]),
            "\x1b[0m\x1b[2J\x1b[1;1Ha中b".as_bytes()
        );
        assert_eq!(text(&mut r, &["a中c"]), b"\x1b[1;4Hc");
    }

    #[test]
    fn a_style_change_on_the_right_half_of_a_wide_glyph_redraws_it() {
        let mut r = Renderer::new(8, 1);
        r.draw(|b| {
            b.set_string(0, 0, "中", Style::new());
        });
        let out = r.draw(|b| {
            b.set_string(0, 0, "中", Style::new());
            b.set_style(Rect::new(1, 0, 1, 1), Style::new().bg(Color::Red));
        });
        assert_eq!(out, "\x1b[1;1H\x1b[0;41m中\x1b[0m".as_bytes());
    }

    #[test]
    fn a_closure_that_resizes_the_buffer_gets_a_blank_frame_not_a_panic() {
        let mut r = Renderer::new(4, 2);
        text(&mut r, &["ab"]);
        let out = r.draw(|b| b.resize(Rect::new(0, 0, 8, 3))).to_vec();
        assert_eq!(out, b"\x1b[1;1H  ");
        assert_eq!(r.area(), Rect::new(0, 0, 4, 2));
        assert!(r.draw(|_| {}).is_empty());
    }

    #[test]
    fn last_column_write_forces_an_absolute_move_next() {
        let mut r = Renderer::new(3, 2);
        assert_eq!(
            text(&mut r, &["abc", "d"]),
            b"\x1b[0m\x1b[2J\x1b[1;1Habc\x1b[2;1Hd"
        );
    }

    #[test]
    fn resize_repaints_everything() {
        let mut r = Renderer::new(4, 1);
        text(&mut r, &["ab"]);
        r.resize(6, 2);
        assert_eq!(r.area(), Rect::new(0, 0, 6, 2));
        assert_eq!(text(&mut r, &["ab"]), b"\x1b[0m\x1b[2J\x1b[1;1Hab");
    }

    #[test]
    fn output_is_deterministic() {
        let frames = [&["hello 中文", "x"][..], &["hello", "y 😀"][..]];
        let run = || {
            let mut r = Renderer::new(12, 2);
            frames
                .iter()
                .flat_map(|f| text(&mut r, f))
                .collect::<Vec<u8>>()
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn every_color_form_produces_valid_sgr() {
        let mut r = Renderer::new(4, 1);
        let out = r
            .draw(|b| {
                let s = Style::new()
                    .fg(Color::Indexed(200))
                    .bg(Color::BrightBlue)
                    .italic()
                    .underline();
                b.set_string(0, 0, "x", s);
            })
            .to_vec();
        assert_eq!(
            out,
            b"\x1b[0m\x1b[2J\x1b[1;1H\x1b[0;3;4;38;5;200;104mx\x1b[0m"
        );
    }

    #[test]
    fn invalidate_repaints_exactly_like_a_first_frame() {
        let mut r = Renderer::new(8, 2);
        let first = text(&mut r, &["ab中", "c"]);
        assert!(text(&mut r, &["ab中", "c"]).is_empty());
        r.invalidate();
        assert_eq!(text(&mut r, &["ab中", "c"]), first);
        assert!(text(&mut r, &["ab中", "c"]).is_empty());
    }

    #[test]
    fn present_skips_the_write_for_an_unchanged_frame() {
        let mut r = Renderer::new(4, 1);
        let mut sink = Vec::new();
        r.present(&mut sink, |b| {
            b.set_string(0, 0, "hi", Style::new());
        })
        .unwrap();
        let len = sink.len();
        r.present(&mut sink, |b| {
            b.set_string(0, 0, "hi", Style::new());
        })
        .unwrap();
        assert_eq!(sink.len(), len);
    }

    /// Accepts `limit` bytes, then fails, like a tty that goes away or a
    /// write that is interrupted halfway through a frame.
    struct Cut {
        sent: Vec<u8>,
        limit: usize,
    }

    impl Write for Cut {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let room = self.limit - self.sent.len();
            if room == 0 {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "cut"));
            }
            let n = room.min(buf.len());
            self.sent.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_frame_cut_at_any_byte_is_repaired_by_the_next_one() {
        let s1 = Style::new().fg(Color::Red).bold();
        let s2 = Style::new().bg(Color::Rgb(10, 20, 30));
        let draw1 = |b: &mut Buffer| {
            b.set_string(0, 0, "hello 中文 x", s1);
            b.set_string(1, 1, "😀ab", s2);
        };
        let draw2 = |b: &mut Buffer| {
            b.set_string(0, 0, "hEllo 字文 y", s2);
            b.set_string(0, 1, "👨‍👩‍👧‍👦 ab", s1);
        };
        let draw3 = |b: &mut Buffer| {
            b.set_string(2, 0, "final 中", s1);
        };
        let mut probe = Renderer::new(12, 2);
        probe.draw(draw1);
        let frame2_len = probe.draw(draw2).len();
        assert!(frame2_len > 20);

        for limit in 0..frame2_len {
            let mut r = Renderer::new(12, 2);
            let mut screen = Screen::new(12, 2);
            let mut first = Vec::new();
            r.present(&mut first, draw1).unwrap();
            screen.feed(&first);

            let mut cut = Cut {
                sent: Vec::new(),
                limit,
            };
            assert!(r.present(&mut cut, draw2).is_err(), "limit {limit}");
            screen.feed(&cut.sent);

            let mut rest = Vec::new();
            r.present(&mut rest, draw3).unwrap();
            screen.feed(&rest);

            let mut want = Buffer::new(r.area());
            draw3(&mut want);
            assert_eq!(screen.to_buffer(), want, "cut after {limit} bytes");
        }
    }

    /// Feeds random frames through the renderer and a small terminal model,
    /// and checks the model's screen equals the buffer after every frame.
    #[test]
    fn screen_matches_buffer_after_random_frames() {
        let pieces = [
            "a",
            "b",
            "é",
            "e\u{301}",
            "中",
            "字",
            "😀",
            "👨‍👩‍👧‍👦",
            "🇧🇷",
            " ",
            "abc",
            "中文",
            "xy",
        ];
        let styles = [
            Style::new(),
            Style::new().fg(Color::Red),
            Style::new().bg(Color::Blue).bold(),
            Style::new().fg(Color::Rgb(9, 8, 7)).underline(),
        ];
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let (w, h) = (9u16, 4u16);
        let mut r = Renderer::new(w, h);
        let mut screen = Screen::new(w, h);
        let mut scene = Buffer::new(Rect::new(0, 0, w, h));
        for frame in 0..400 {
            if frame % 97 == 96 {
                r.resize(w, h);
            }
            // Most frames tweak the previous content a little; that is the
            // case where stale halves of wide glyphs show up.
            let mut ops = Vec::new();
            let mut scene_styles = Vec::new();
            for _ in 0..(next() % 6) {
                let x = (next() % (w as u64 + 2)) as u16;
                let y = (next() % h as u64) as u16;
                let s = pieces[(next() % pieces.len() as u64) as usize];
                let st = styles[(next() % styles.len() as u64) as usize];
                ops.push((x, y, s, st));
                if next() % 3 == 0 {
                    // Style one cell alone: for a wide glyph, either half.
                    scene_styles.push((next() as u16 % w, y, styles[(next() % 4) as usize]));
                }
            }
            if frame == 0 || next() % 4 == 0 {
                scene.reset();
            }
            for (x, y, s, st) in ops {
                scene.set_string(x, y, s, st);
            }
            for (x, y, st) in scene_styles {
                scene.set_style(Rect::new(x, y, 1, 1), st);
            }
            let bytes = r.draw(|b| *b = scene.clone()).to_vec();
            screen.feed(&bytes);
            assert_eq!(screen.to_buffer(), scene, "frame {frame}: screen diverged");
        }
    }
    fn with_cursor(r: &mut Renderer, rows: &[&str], at: Option<(u16, u16)>) -> Vec<u8> {
        r.draw_frame(|f| {
            for (y, row) in rows.iter().enumerate() {
                f.buffer_mut().set_string(0, y as u16, row, Style::new());
            }
            if let Some((x, y)) = at {
                f.set_cursor(x, y);
            }
        })
        .to_vec()
    }

    #[test]
    fn a_frame_without_a_cursor_leaves_it_hidden_and_writes_nothing_extra() {
        let mut r = Renderer::new(6, 2);
        let mut screen = Screen::new(6, 2);
        let out = with_cursor(&mut r, &["ab"], None);
        assert!(!out.windows(6).any(|w| w == b"\x1b[?25h"));
        screen.feed(&out);
        assert!(with_cursor(&mut r, &["ab"], None).is_empty());
    }

    #[test]
    fn the_cursor_is_shown_where_the_frame_asks_for_it() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((4, 2))));
        assert_eq!(screen.cursor(), ((4, 2), true));
        assert_eq!(screen.row(0), "ab    ");
    }

    #[test]
    fn a_cursor_that_stays_put_on_an_unchanged_frame_costs_no_bytes() {
        let mut r = Renderer::new(6, 3);
        with_cursor(&mut r, &["ab"], Some((4, 2)));
        assert!(with_cursor(&mut r, &["ab"], Some((4, 2))).is_empty());
    }

    #[test]
    fn moving_only_the_cursor_writes_just_the_move() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((4, 2))));
        let out = with_cursor(&mut r, &["ab"], Some((1, 0)));
        assert_eq!(out, b"\x1b[1;2H");
        screen.feed(&out);
        assert_eq!(screen.cursor(), ((1, 0), true));
    }

    #[test]
    fn writing_cells_puts_the_cursor_back_where_the_frame_wants_it() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((2, 0))));
        // The cell writes move the terminal's cursor; the frame's cursor
        // must be placed again afterwards even though it did not change.
        screen.feed(&with_cursor(&mut r, &["ab", "cd"], Some((2, 0))));
        assert_eq!(screen.cursor(), ((2, 0), true));
        assert_eq!(screen.row(1), "cd    ");
    }

    #[test]
    fn dropping_the_cursor_hides_it_once() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((2, 0))));
        let out = with_cursor(&mut r, &["ab"], None);
        assert_eq!(out, b"\x1b[?25l");
        screen.feed(&out);
        assert!(!screen.cursor().1);
        assert!(with_cursor(&mut r, &["ab"], None).is_empty());
    }

    #[test]
    fn a_cursor_outside_the_frame_counts_as_none() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((1, 1))));
        assert!(screen.cursor().1);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((6, 0))));
        assert!(!screen.cursor().1);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((0, 3))));
        assert!(!screen.cursor().1);
    }

    #[test]
    fn a_reset_terminal_gets_the_cursor_shown_again() {
        let mut r = Renderer::new(6, 3);
        with_cursor(&mut r, &["ab"], Some((2, 0)));
        r.cursor_was_reset();
        r.invalidate();
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((2, 0))));
        assert_eq!(screen.cursor(), ((2, 0), true));
    }

    #[test]
    fn the_cursor_survives_random_frames_and_lands_where_asked() {
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut next = move |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        let (w, h) = (7u16, 3u16);
        let mut r = Renderer::new(w, h);
        let mut screen = Screen::new(w, h);
        for frame in 0..500 {
            let rows: Vec<String> = (0..h)
                .map(|_| ["", "ab", "中c", "abcdefg"][next(4) as usize].to_string())
                .collect();
            let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
            let at = (next(3) != 0).then(|| (next(w as u64 + 1) as u16, next(h as u64 + 1) as u16));
            screen.feed(&with_cursor(&mut r, &refs, at));
            let inside = at.filter(|&(x, y)| x < w && y < h);
            match inside {
                Some((x, y)) => {
                    assert_eq!(
                        screen.cursor(),
                        ((x as usize, y as usize), true),
                        "frame {frame}"
                    )
                }
                None => assert!(!screen.cursor().1, "frame {frame}"),
            }
        }
    }

    #[test]
    fn a_cursor_the_terminal_lost_is_shown_again_without_a_repaint() {
        let mut r = Renderer::new(6, 3);
        let mut screen = Screen::new(6, 3);
        screen.feed(&with_cursor(&mut r, &["ab"], Some((2, 0))));
        screen.feed(b"\x1b[?25l");
        r.cursor_was_reset();
        screen.feed(&with_cursor(&mut r, &["ab"], Some((2, 0))));
        assert_eq!(screen.cursor(), ((2, 0), true));
    }
}

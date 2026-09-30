use super::Widget;
use crate::text::{truncate, width};
use crate::{Buffer, Rect, Style};

/// A frame of animation that moves when the app says so.
///
/// The spinner has no clock. The app keeps it in its state, sends itself a
/// message from a timer (see `Cmd::after`), and calls [`Spinner::tick`] when
/// it arrives. The widget draws whichever frame is current, in the first
/// cell of its area.
///
/// ```
/// use crewtui::widgets::Spinner;
///
/// let mut spinner = Spinner::dots();
/// let first = spinner.frame();
/// spinner.tick();
/// assert_ne!(spinner.frame(), first);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spinner {
    frames: &'static [&'static str],
    index: usize,
    style: Style,
}

impl Spinner {
    const DOTS: [&'static str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    const LINE: [&'static str; 4] = ["-", "\\", "|", "/"];

    /// Braille dots going around.
    pub fn dots() -> Self {
        Spinner::with_frames(&Spinner::DOTS)
    }

    /// A rotating line, for terminals without braille.
    pub fn line() -> Self {
        Spinner::with_frames(&Spinner::LINE)
    }

    /// A spinner over your own frames. With no frames it draws nothing.
    pub fn with_frames(frames: &'static [&'static str]) -> Self {
        Spinner {
            frames,
            index: 0,
            style: Style::new(),
        }
    }

    /// The style the frame is drawn in.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Moves to the next frame, wrapping around.
    pub fn tick(&mut self) {
        if !self.frames.is_empty() {
            self.index = (self.index + 1) % self.frames.len();
        }
    }

    /// The current frame, or an empty string if there are none.
    pub fn frame(&self) -> &'static str {
        self.frames.get(self.index).copied().unwrap_or("")
    }
}

impl Widget for Spinner {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        let frame = truncate(self.frame(), usize::from(area.width));
        if width(frame) > 0 {
            buf.set_string(area.x, area.y, frame, self.style);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    #[test]
    fn ticking_walks_the_frames_and_wraps() {
        let mut s = Spinner::line();
        let seen: Vec<_> = (0..9)
            .map(|_| {
                let f = s.frame();
                s.tick();
                f
            })
            .collect();
        assert_eq!(seen, ["-", "\\", "|", "/", "-", "\\", "|", "/", "-"]);
    }

    #[test]
    fn a_spinner_with_no_frames_neither_panics_nor_draws() {
        let mut s = Spinner::with_frames(&[]);
        s.tick();
        assert_eq!(s.frame(), "");
        let area = Rect::new(0, 0, 3, 1);
        let mut buf = Buffer::new(area);
        s.render(area, &mut buf);
        assert_eq!(buf.get(0, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn it_draws_the_current_frame_in_the_first_cell_with_its_style() {
        let area = Rect::new(2, 1, 3, 2);
        let mut buf = Buffer::new(Rect::new(0, 0, 8, 4));
        let mut s = Spinner::dots().style(Style::new().fg(Color::Cyan));
        s.tick();
        s.render(area, &mut buf);
        assert_eq!(buf.get(2, 1).unwrap().symbol(), "⠙");
        assert_eq!(buf.get(2, 1).unwrap().style().fg, Some(Color::Cyan));
        assert_eq!(buf.get(3, 1).unwrap().symbol(), " ");
    }

    #[test]
    fn a_wide_frame_is_cut_by_columns() {
        static WIDE: [&str; 1] = ["中文"];
        let mut buf = Buffer::new(Rect::new(0, 0, 4, 1));
        Spinner::with_frames(&WIDE).render(Rect::new(0, 0, 3, 1), &mut buf);
        assert_eq!(buf.get(0, 0).unwrap().symbol(), "中");
        assert_eq!(buf.get(2, 0).unwrap().symbol(), " ");
        let mut narrow = Buffer::new(Rect::new(0, 0, 4, 1));
        Spinner::with_frames(&WIDE).render(Rect::new(0, 0, 1, 1), &mut narrow);
        assert_eq!(narrow.get(0, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn it_is_cheap_to_copy_and_empty_areas_are_fine() {
        let s = Spinner::dots();
        let mut copy = s;
        copy.tick();
        assert_ne!(s.frame(), copy.frame());
        let mut buf = Buffer::new(Rect::new(0, 0, 2, 2));
        s.render(Rect::new(0, 0, 0, 0), &mut buf);
        s.render(Rect::new(9, 9, 2, 2), &mut buf);
        assert!(buf.cells().iter().all(|c| c.symbol() == " "));
    }
}

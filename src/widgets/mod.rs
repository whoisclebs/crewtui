//! Things that draw into a [`Buffer`].
//!
//! A widget is a value you build for one frame and hand to
//! [`Frame::render_widget`](crate::Frame::render_widget). It draws inside the
//! area it is given and keeps no state of its own; anything that persists
//! between frames, such as a scroll position, lives in a state value the app
//! owns.

mod paragraph;

pub use paragraph::{Paragraph, Wrap};

use crate::text::{Line, Span, Text};
use crate::{Buffer, Rect};

/// Draws itself into a rectangle of a buffer.
///
/// `area` is where the widget may draw. Cells outside it are left alone, and
/// an empty area is fine.
pub trait Widget {
    /// Draws the widget.
    fn render(self, area: Rect, buf: &mut Buffer);
}

impl Widget for Text<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self).render(area, buf);
    }
}

impl Widget for Line<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self).render(area, buf);
    }
}

impl Widget for Span<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self).render(area, buf);
    }
}

impl Widget for &str {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self).render(area, buf);
    }
}

impl Widget for String {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self).render(area, buf);
    }
}

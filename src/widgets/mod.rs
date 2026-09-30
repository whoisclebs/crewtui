//! Things that draw into a [`Buffer`].
//!
//! A widget is a value you build for one frame and hand to
//! [`Frame::render_widget`](crate::Frame::render_widget). It draws inside the
//! area it is given and keeps no state of its own; anything that persists
//! between frames, such as a scroll position, lives in a state value the app
//! owns.

mod block;
mod list;
mod paragraph;
mod progress;
mod scrollbar;
mod spinner;

pub use block::{Block, BorderType, Borders};
pub use list::{List, ListItem, ListState};
pub use paragraph::{Paragraph, Wrap};
pub use progress::Progress;
pub use scrollbar::{Orientation, Scrollbar};
pub use spinner::Spinner;

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

/// A widget that needs state that outlives the frame, such as a selection.
///
/// The state is a value the app owns and passes back each frame. It is only
/// borrowed shared, so a widget can be drawn from `App::view`, which can't
/// change the app. What the app decides, such as which item is selected,
/// is plain data changed in `update`. What only drawing can know, such as
/// how far a list has scrolled to keep the selection in view, is kept in a
/// `Cell` inside the state, so it is still there next frame.
pub trait StatefulWidget {
    /// The state this widget reads.
    type State;

    /// Draws the widget.
    fn render(self, area: Rect, buf: &mut Buffer, state: &Self::State);
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

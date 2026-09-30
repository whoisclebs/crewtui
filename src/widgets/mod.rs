//! Things that draw into a [`Buffer`].
//!
//! A widget is a value you build for one frame and hand to
//! [`Frame::render_widget`](crate::Frame::render_widget). It draws inside the
//! area it is given and keeps no state of its own; anything that persists
//! between frames, such as a scroll position, lives in a state value the app
//! owns.
//!
//! The state types (`ListState`, `TableState`, `InputState`, `HistoryState`)
//! follow one convention. `new()` is empty, and a `with_*` constructor takes
//! the initial content: `InputState::with_text`, `ListState::with_selected`,
//! `HistoryState::with_entries`. Afterwards the app changes them with `&mut`
//! methods in `update`: `set_*`, `select*`, `push`, `insert_*`. Methods that
//! take text accept anything `AsRef<str>`. Selection helpers such as
//! `select_next(len)` take the length of the collection, since the state
//! doesn't hold it.
//!
//! What drawing works out, like a scroll offset, is kept in a `Cell` inside
//! the state so that `view`, which only has `&self`, can remember it. That
//! makes the state types `!Sync`: they belong to the thread that runs the
//! app. A state that other threads need to read has to be cloned into
//! something they can share.

mod block;
mod clear;
mod history;
mod input;
mod list;
mod paragraph;
mod progress;
mod scrollbar;
mod spinner;
mod table;

pub use block::{Block, BorderType, Borders};
pub use clear::Clear;
pub use history::{History, HistoryState};
pub use input::{Input, InputState};
pub use list::{List, ListItem, ListState};
pub use paragraph::{Paragraph, Wrap};
pub use progress::Progress;
pub use scrollbar::{Orientation, Scrollbar};
pub use spinner::Spinner;
pub use table::{Row, Table, TableState};

use crate::text::{Line, Span, Text};
use crate::{Buffer, Rect};

/// The smallest offset that keeps item `selected` in a viewport of `rows`
/// rows, walking up from it and no further than the viewport is tall.
/// `height(i)` is how many rows item `i` takes.
pub(crate) fn offset_for(height: impl Fn(usize) -> usize, selected: usize, rows: usize) -> usize {
    let mut used = height(selected);
    let mut offset = selected;
    while offset > 0 && used + height(offset - 1) <= rows {
        offset -= 1;
        used += height(offset);
    }
    offset
}

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

    /// Where the terminal's own cursor goes after this widget is drawn, if it
    /// has one, such as the text cursor of an input. It is asked after
    /// [`StatefulWidget::render`], so the state can hold where drawing put
    /// it. [`Frame::render_stateful_widget`](crate::Frame::render_stateful_widget)
    /// passes it on to [`Frame::set_cursor`](crate::Frame::set_cursor). Most
    /// widgets have none.
    fn cursor(_state: &Self::State) -> Option<(u16, u16)> {
        None
    }
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

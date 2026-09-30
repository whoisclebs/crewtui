//! What a view draws into.

use crate::widgets::{StatefulWidget, Widget};
use crate::{Buffer, Rect};

/// The surface [`App::view`](crate::App::view) draws on for one frame.
///
/// It starts blank and covers the whole terminal. Widgets render into its
/// buffer; nothing reaches the terminal until the view returns and the
/// renderer has diffed the result against the previous frame.
#[derive(Debug)]
pub struct Frame<'a> {
    buffer: &'a mut Buffer,
}

impl<'a> Frame<'a> {
    pub(crate) fn new(buffer: &'a mut Buffer) -> Self {
        Frame { buffer }
    }

    /// The area of the whole terminal.
    pub fn area(&self) -> Rect {
        self.buffer.area()
    }

    /// Draws `widget` inside `area`. Whatever falls outside the frame is
    /// clipped.
    pub fn render_widget(&mut self, widget: impl Widget, area: Rect) {
        widget.render(area.intersection(self.buffer.area()), self.buffer);
    }

    /// Draws a widget that keeps `state` between frames, inside `area`.
    pub fn render_stateful_widget<W: StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &W::State,
    ) {
        widget.render(area.intersection(self.buffer.area()), self.buffer, state);
    }

    /// The buffer to draw into.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

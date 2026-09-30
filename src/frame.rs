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
    cursor: Option<(u16, u16)>,
}

impl<'a> Frame<'a> {
    pub(crate) fn new(buffer: &'a mut Buffer) -> Self {
        Frame {
            buffer,
            cursor: None,
        }
    }

    /// Where the terminal's own cursor goes once the frame is drawn. A frame
    /// that doesn't ask for one has no visible cursor. A position outside the
    /// frame hides it too.
    pub fn set_cursor(&mut self, x: u16, y: u16) {
        self.cursor = Some((x, y));
    }

    pub(crate) fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
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

    /// Draws a widget that keeps `state` between frames, inside `area`. If
    /// the widget has a cursor, like an [`Input`](crate::widgets::Input), the
    /// terminal cursor is placed there.
    pub fn render_stateful_widget<W: StatefulWidget>(
        &mut self,
        widget: W,
        area: Rect,
        state: &W::State,
    ) {
        widget.render(area.intersection(self.buffer.area()), self.buffer, state);
        if let Some((x, y)) = W::cursor(state) {
            self.set_cursor(x, y);
        }
    }

    /// The buffer to draw into.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

//! What a view draws into.

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

    /// The buffer to draw into.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        self.buffer
    }
}

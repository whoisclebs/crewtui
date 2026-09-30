//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

mod buffer;
mod geometry;
mod style;

pub use buffer::{Buffer, Cell};
pub use geometry::Rect;
pub use style::{Color, Modifier, Style};

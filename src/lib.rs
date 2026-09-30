//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

mod buffer;
mod geometry;
mod render;
mod style;
#[cfg(unix)]
mod terminal;
#[cfg(test)]
mod testing;
pub mod text;

pub use buffer::{Buffer, Cell};
pub use geometry::Rect;
pub use render::Renderer;
pub use style::{Color, Modifier, Style};
#[cfg(unix)]
pub use terminal::{Terminal, TerminalOptions};

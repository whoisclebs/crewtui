//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

mod buffer;
mod geometry;
mod input;
mod render;
#[cfg(unix)]
mod signals;
mod style;
#[cfg(unix)]
mod terminal;
#[cfg(test)]
mod testing;
pub mod text;

pub use buffer::{Buffer, Cell};
pub use geometry::Rect;
pub use input::{Event, KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind, Parser};
pub use render::Renderer;
#[cfg(unix)]
pub use signals::{Signal, Signals};
pub use style::{Color, Modifier, Style};
#[cfg(unix)]
pub use terminal::{Terminal, TerminalOptions};

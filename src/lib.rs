//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

mod buffer;
mod frame;
mod geometry;
mod input;
#[cfg(unix)]
mod reader;
mod render;
#[cfg(unix)]
mod runtime;
#[cfg(unix)]
mod signals;
mod style;
#[cfg(unix)]
mod terminal;
#[cfg(test)]
mod testing;
pub mod text;

pub use buffer::{Buffer, Cell};
pub use frame::Frame;
pub use geometry::Rect;
pub use input::{Event, KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind, Parser};
pub use render::Renderer;
#[cfg(unix)]
pub use runtime::{App, Cmd, Program};
#[cfg(unix)]
pub use signals::{Signal, Signals};
pub use style::{Color, Modifier, Style};
#[cfg(unix)]
pub use terminal::{Terminal, TerminalOptions};

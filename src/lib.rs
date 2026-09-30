//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

#[cfg(all(test, unix))]
mod agent_tests;
mod buffer;
#[cfg(unix)]
mod effects;
mod frame;
mod geometry;
mod input;
mod layout;
#[cfg(all(test, unix))]
mod pty_children;
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
pub mod widgets;

// The reference app is built as an example, and also compiled here so the
// tests can drive it. The example names the crate as `crewtui`.
#[cfg(all(test, unix))]
extern crate self as crewtui;
#[cfg(all(test, unix))]
#[allow(dead_code)]
#[path = "../examples/agent.rs"]
mod agent_example;

pub use buffer::{Buffer, Cell};
#[cfg(unix)]
pub use effects::{Closed, Sender};
pub use frame::Frame;
pub use geometry::Rect;
pub use input::{Event, KeyCode, KeyEvent, Modifiers, MouseButton, MouseEvent, MouseKind, Parser};
pub use layout::{Alignment, Constraint, Edges, Layout};
pub use render::Renderer;
#[cfg(unix)]
pub use runtime::{App, Cmd, Program};
#[cfg(unix)]
pub use signals::{Signal, Signals};
pub use style::{Color, Modifier, Style};
#[cfg(unix)]
pub use terminal::{Terminal, TerminalOptions};

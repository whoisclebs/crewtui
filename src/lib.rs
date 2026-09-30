//! CrewTUI: a small terminal UI framework built around state, messages,
//! update and view.

#[cfg(all(test, unix))]
mod agent_tests;
mod buffer;
mod clipboard;
mod effects;
mod frame;
mod geometry;
mod input;
mod layout;
mod options;
#[cfg(all(test, unix))]
mod pty_children;
#[cfg(unix)]
mod reader;
#[cfg(windows)]
#[path = "reader_windows.rs"]
mod reader;
mod render;
mod runtime;
mod signal;
#[cfg(unix)]
mod signals;
#[cfg(windows)]
#[path = "signals_windows.rs"]
mod signals;
mod style;
#[cfg(test)]
mod term_model;
#[cfg(unix)]
mod terminal;
#[cfg(windows)]
#[path = "terminal_windows.rs"]
mod terminal;
pub mod testing;
pub mod text;
mod utf16;
pub mod widgets;

// The reference app is built as an example, and also compiled here so the
// tests can drive it. The example names the crate as `crewtui`.
#[cfg(all(test, unix))]
extern crate self as crewtui;
#[cfg(all(test, unix))]
#[allow(dead_code)]
#[path = "../examples/agent.rs"]
mod agent_example;

// The README and the getting-started guide are compiled as doctests, so their
// code can't drift from the API.
#[cfg(all(doctest, unix))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
#[cfg(all(doctest, unix))]
#[doc = include_str!("../docs/getting-started.md")]
struct GettingStartedDoctests;

pub use buffer::{Buffer, Cell};
pub use clipboard::CLIPBOARD_LIMIT;
pub use effects::{Closed, Sender};
pub use frame::Frame;
pub use geometry::Rect;
pub use input::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseKind,
    Parser,
};
pub use layout::{Constraint, Edges, Justify, Layout};
pub use options::TerminalOptions;
pub use render::Renderer;
pub use runtime::{App, Cmd, Program, run};
pub use signal::Signal;
pub(crate) use signals::Signals;
pub use style::{Color, Modifier, Style};
pub use terminal::Terminal;

/// The names most apps import, in one `use crewtui::prelude::*;`.
pub mod prelude {
    pub use crate::widgets::{Block, Paragraph, StatefulWidget, Widget};
    pub use crate::{App, Cmd, Program};
    pub use crate::{Color, Constraint, Event, Frame, KeyCode, KeyEvent, KeyModifiers, Layout};
    pub use crate::{Rect, Style};
}

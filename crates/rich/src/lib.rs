//! Markdown, code and diffs as CrewTUI text.
//!
//! Each of these turns a string into a [`Text`](crewtui::text::Text) with
//! styles and links, which any widget that takes text can show: a
//! `Paragraph`, or an entry of a `History`. Each also is a widget that draws
//! that text itself. Nothing here needs more than CrewTUI.
//!
//! - [`Markdown`] renders a subset of Markdown: headings, emphasis, inline
//!   code, links, lists, quotes, rules and fenced code.
//! - [`CodeBlock`] draws code with line numbers and simple syntax
//!   highlighting, and [`highlight`] gives the highlighted lines.
//! - [`Diff`] colors a unified diff and can number its lines.
//!
//! The highlighter is a small scanner for keywords, strings, comments and
//! numbers in a dozen common languages. It is not a parser, and it does not
//! aim to match an editor's highlighting.

mod code;
mod diff;
mod markdown;

pub use code::{CodeBlock, HighlightStyles, highlight};
pub use diff::{Diff, DiffStyles};
pub use markdown::{Markdown, MarkdownStyles};

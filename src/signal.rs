//! The signals the loop reports, on every platform.

use std::fmt;

/// A signal the framework cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Signal {
    /// SIGINT, sent from outside; on Windows, Ctrl+Break. Typing Ctrl+C in
    /// raw mode doesn't produce it; that arrives as a key event.
    Interrupt,
    /// SIGTERM; on Windows, the system shutting down or the user logging off,
    /// which Windows only reports to services.
    Terminate,
    /// SIGHUP, for example when the terminal window closes; on Windows, the
    /// console window being closed.
    Hangup,
    /// SIGWINCH: the terminal was resized. On Windows, the console buffer or
    /// the window on it changed size.
    Resize,
    /// SIGCONT: the process resumed after being stopped.
    Continue,
    /// SIGTSTP, sent from outside, for example by a job-control shell. Typing
    /// Ctrl+Z in raw mode doesn't produce it; that arrives as a key event.
    /// The loop restores the terminal, stops the process, and takes the
    /// terminal again when it is continued.
    Suspend,
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Signal::Interrupt => "interrupted (SIGINT)",
            Signal::Terminate => "terminated (SIGTERM)",
            Signal::Hangup => "hung up (SIGHUP)",
            Signal::Resize => "terminal resized (SIGWINCH)",
            Signal::Continue => "continued (SIGCONT)",
            Signal::Suspend => "stopped (SIGTSTP)",
        })
    }
}

impl std::error::Error for Signal {}

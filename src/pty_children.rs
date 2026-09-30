//! Scenarios that run as a child process on a pty.
//!
//! The parent test calls `Pty::spawn_self("pty_children::child_entry", mode)`,
//! which reruns this test executable with only `child_entry` selected and
//! `CREWTUI_PTY_CHILD` set. When the variable is not set, as in a normal
//! test run, `child_entry` does nothing and passes.

use crate::testing::CHILD_MODE;

/// The entry point of the child. It never returns once it has a mode: it
/// plays the scenario and exits the process.
#[test]
fn child_entry() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    match mode.as_str() {
        "hello" => println!("hello from the child"),
        other => panic!("unknown pty child mode {other:?}"),
    }
    std::process::exit(0);
}

# Testing terminal behavior

Most of the crate is tested with plain unit tests. The parts that talk to a terminal need something that behaves like one. Two helpers in `src/testing.rs` (compiled only for tests) do that.

## The terminal model

`Screen` applies the escape sequences the renderer emits and exposes the grid. It handles cursor moves, colors and attributes, clearing, the pending wrap after the last column, and erasing the other half of a wide glyph that gets overwritten. It parses like a real terminal: sequences can arrive in pieces, an ESC inside a CSI sequence aborts it, and invalid UTF-8 prints U+FFFD. Anything it doesn't model panics, so a new escape sequence in the output fails a test instead of being ignored.

Use it to check what a program put on screen: feed it bytes, then read `row(y)` or compare `to_buffer()` with the buffer the app drew.

## The pty

`Pty` is a pseudo-terminal pair. The program under test gets the slave side; the test reads and writes the master side. That gives real termios, real window sizes, real hangups.

- `open()` makes a pair. Neither descriptor is inherited by children.
- `set_size(columns, rows)`, `output()` (everything written so far), `termios()` and `is_raw()`.
- `spawn(&mut Command)` runs a process with the pty as stdin, stdout, stderr and controlling terminal, in its own session. That is what makes `close_master()` hang it up and lets the terminal generate signals for it.
- `wait_timeout(&mut child, limit)` waits for it to exit and returns `None` if it doesn't.

A test that runs a program on a pty records `termios()` before, runs it, and compares afterwards with `same()`. `sh -c 'stty raw -echo'` is a program that leaves the terminal broken on purpose, and one of the harness's own tests uses it to show the comparison notices.

## Running a scenario as a child process

Some behavior can't be checked in the test process: a real SIGTERM, a hangup, a panic with the default hook. Those tests rerun the test executable itself.

`Pty::spawn_self("pty_children::child_entry", mode)` starts the same executable with only that one test selected and `CREWTUI_PTY_CHILD=mode` in the environment. `child_entry` in `src/pty_children.rs` reads the mode, plays the scenario on its stdin and stdout (which are the pty), and exits. In a normal run the variable isn't set and the test passes without doing anything.

To add a scenario, add a match arm in `child_entry` and a test that spawns it, waits, sends whatever it should send, and checks the termios and the bytes on the master.

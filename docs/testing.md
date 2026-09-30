# Testing terminal behavior

Most of the crate is tested with plain unit tests. The parts that talk to a terminal need something that behaves like one. Two helpers in `src/term_model.rs` (compiled only for tests) do that. An app's own tests use a third, public one, the harness below.

## Testing an app: `testing::Harness`

`crewtui::testing::Harness` runs an `App` with no terminal, no threads and no clock, for the tests of the app that uses the crate. It lives in the public API and depends on nothing.

`Harness::new(app, width, height)` wraps the app. `start()` runs `App::init`, `event(Event)` and `key(KeyEvent)` go through the app's `event` and `update`, `message(msg)` goes straight to `update`, and `resize(w, h)` changes the size and sends `Event::Resize`. `screen()` draws the app and returns a `Snapshot` with `rows()` (trailing spaces trimmed), `text()`, `contains(&str)`, `cell(x, y)` for symbol and style, `link_at(x, y)` and `cursor()`.

What commands ask for is visible to the test instead of happening elsewhere. `Cmd::perform` and `Cmd::spawn` wait until `run_commands()`, which runs them on the test thread and delivers their messages, including the work those start. `Cmd::after` waits until `fire_timers()`, and `pending_timers()` lists the delays. `Cmd::copy_to_clipboard` texts are in `clipboard()`, and `Cmd::quit` sets `has_quit()`. A test decides when each of these happens, so nothing depends on timing.

## The terminal model

`Screen` applies the escape sequences the renderer emits and exposes the grid. It handles cursor moves, colors and attributes, hyperlinks (OSC 8, kept apart from the cells), clearing, the pending wrap after the last column, and erasing the other half of a wide glyph that gets overwritten. It parses like a real terminal: sequences can arrive in pieces, an ESC inside a CSI or OSC sequence aborts it, and invalid UTF-8 prints U+FFFD. Anything it doesn't model panics, so a new escape sequence in the output fails a test instead of being ignored.

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

## What the safety tests cover

`src/pty_children.rs` plays out every way a program can leave, each as its own process on its own pty. After each one the test checks that termios is back to what it was before, and that the bytes the child wrote include the sequence that leaves the alternate screen, shows the cursor and turns the reporting modes off.

- Quitting normally, with the default modes and with mouse, focus and paste reporting all on.
- Ctrl+C typed in raw mode, which arrives as a key event and quits the app, not as a signal.
- Returning an error early with `?` while holding a `Terminal`.
- A panic in `update` and a panic in `view`. The panic message has to appear after the sequence that leaves the alternate screen, or it would be lost on the screen that is about to disappear.
- A panic with the `Terminal` guard leaked, which stands in for `panic = "abort"`: no destructor runs, so only the panic hook can put the terminal back.
- SIGTERM, SIGINT and SIGHUP sent from outside, each ending the run with an error that names the signal.
- The terminal itself going away. There is nothing left to restore, so the check is that the process stops through the error path instead of hanging.

These use real signals and hangups, which is why they run as child processes: a SIGTERM aimed at the test process itself would end the whole test run.


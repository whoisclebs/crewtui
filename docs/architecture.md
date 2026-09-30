# Architecture

A message goes through the app in one direction:

```text
terminal bytes -> Parser -> Event -> App::event -> Message
                                                      |
                                       App::update (state, returns a Cmd)
                                                      |
             Cmd runs on workers, sends Messages back |
                                                      v
                 App::view -> Frame -> Buffer -> diff against previous -> bytes -> terminal
```

`Program::run` owns everything that touches the terminal. The app owns its state and the three methods. `update` is the only place state changes, `view` only reads it, and effects report back with messages, so the same state draws the same frame. The sections below cover the renderer, the loop, effects, widgets, text and the terminal lifecycle. The decisions that would be costly to reverse are in [`docs/adr/`](adr/).

## Renderer and recovery

The renderer keeps two buffers. Each frame the view draws into `current`, the renderer diffs it against `previous`, returns the bytes that turn one into the other, and swaps them.

`previous` is a belief about what the terminal shows, not a fact. It stops being true when:

- a write fails or is cut short halfway through a frame;
- another process writes to the tty, or a multiplexer redraws the screen;
- the process is suspended and resumed;
- a child process takes over the terminal and gives it back;
- the terminal is resized.

A diff against a wrong `previous` doesn't fail, it just leaves garbage on screen that nothing ever corrects. So there is one recovery path: `Renderer::invalidate()`. The next frame starts with a clear screen and paints every non-blank cell, as if it were the first frame.

Who calls it:

- `Renderer::resize` does, by itself.
- `Renderer::present` and `present_frame` do when their write returns an error, whether or not some bytes went through. That matters to code that drives a `Renderer` itself and carries on after the error. `Program` doesn't: a failed write ends the run.
- The runtime does after SIGCONT.
- Applications ask for it with `Cmd::repaint` (Ctrl+L in the example app), for instance after a child process used the terminal.

The tests cut a frame at every possible byte, including inside an escape sequence, feed the pieces to a small terminal model, and check that the next frame leaves the screen equal to the buffer.

## The loop

Besides the three app methods, `Program::run` holds the raw-mode guard, a reader thread, the signal handlers and the renderer.

The main thread blocks in one place, a channel receiver. The reader thread waits on stdin, the signal pipe and a wake-up pipe together, parses input with `Parser`, and sends events and signals into that channel. Work started by effects sends its messages into the same channel, so nothing wakes the loop by polling on a timer.

When something changes, the loop draws, but no more than `max_fps` times a second. It applies everything that is already waiting before it draws, so a burst of messages costs one frame. Quitting doesn't draw a last frame.

Signals are turned into loop behavior in one place. SIGWINCH resizes the renderer, which repaints, and gives the app an `Event::Resize`. SIGCONT re-enters raw mode and invalidates the renderer. SIGINT, SIGTERM and SIGHUP end the run with an `Interrupted` error that wraps the signal, and the terminal is restored on the way out like on any other exit. Ctrl+C typed in raw mode is not a signal: it reaches the app as a key event, and the app decides whether it quits.

The loop itself is a function over a small `Host` trait (write bytes, report the size, resume raw mode). That is what lets the tests drive it without a terminal, with one test that runs a real `Program` on a pty.

## Effects

`update` returns a `Cmd` and never does the work itself. `Cmd::perform` runs a blocking closure on a worker thread and turns its result into a message; `Cmd::spawn` gives a worker a `Sender` for work that produces many messages, like a token stream; `Cmd::after` delivers a message later; `Cmd::repaint` invalidates the renderer; `Cmd::batch` and `Cmd::quit` do what they say. Apps that have their own threads or a runtime use `Program::sender()`.

The pool is bounded, timers share one thread, and everything that hasn't started is dropped when the program ends. The reasoning and the alternatives are in `docs/adr/0001-executor.md`.

## Widgets

A widget is a value built inside `view` and consumed by drawing it into a rectangle of the buffer: `Paragraph::new(text)`, `List::new(items)`, `Block::bordered()`. `Frame::render_widget` takes the widget and an area, and `Layout` splits an area into pieces with fixed, fill, percentage, min and max constraints, plus padding, margin, gap and alignment. Widgets never see the terminal.

State that has to outlive a frame stays with the app. `List` takes a `ListState` holding the selection, `Input` an `InputState` with the text and cursor, `History` a `HistoryState` with the entries. What the app decides is plain data changed in `update`. What only drawing can know, like the scroll offset that keeps the selection visible, is derived while drawing and kept in a `Cell` inside the state. See [ADR 5](adr/0005-widget-ownership.md).

`History` is the widget for a long transcript. It keeps the number of rows of every entry at the current width and sums them lazily, so a frame wraps and draws only what is on screen. Appending to the last entry measures only that entry. Numbers are in [performance](perf.md).

## Text

The unit of the buffer is a grapheme cluster, not a `char` or a byte, and its width in columns comes from `unicode-width`. Wide glyphs take two cells, combining marks join the cell before them, and truncation, wrapping and the input cursor all cut on cluster boundaries. [ADR 4](adr/0004-unicode.md) has the rules and the known gaps.

## Terminal lifecycle

`Terminal` enters raw mode and the modes selected in `TerminalOptions` (alternate screen, hidden cursor, mouse, focus reports, bracketed paste) and undoes them when it is dropped, so early returns and `?` restore the terminal too. Restoring is idempotent and reports the first error it hits without skipping the rest of the steps. A panic hook restores the terminal of the panicking thread before the message is printed, so the message lands on the normal screen and not the alternate one. SIGINT, SIGTERM and SIGHUP end the run with an error and the same restore (see the loop above).

The tests for all of this run a real child process on a pty and check the terminal modes and the termios settings afterwards; see [testing](testing.md).

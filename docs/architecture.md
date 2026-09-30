# Architecture

This file grows with the code. Right now it covers the renderer; the event
loop, effects and widgets are added as they land (#23).

## Renderer and recovery

The renderer keeps two buffers. Each frame the view draws into `current`,
the renderer diffs it against `previous`, returns the bytes that turn one
into the other, and swaps them.

`previous` is a belief about what the terminal shows, not a fact. It stops
being true when:

- a write fails or is cut short halfway through a frame;
- another process writes to the tty, or a multiplexer redraws the screen;
- the process is suspended and resumed;
- a child process takes over the terminal and gives it back;
- the terminal is resized.

A diff against a wrong `previous` doesn't fail, it just leaves garbage on
screen that nothing ever corrects. So there is one recovery path:
`Renderer::invalidate()`. The next frame starts with a clear screen and
paints every non-blank cell, as if it were the first frame.

Who calls it:

- `Renderer::resize` does, by itself.
- `Renderer::present` does when its write returns an error, whether or not
  some bytes went through.
- The runtime will call it after SIGCONT and after running an external process (#7, #9),
  and applications will be able to ask for it with a repaint command (#10;
  Ctrl+L in the example app).

The tests cut a frame at every possible byte, including inside an escape
sequence, feed the pieces to a small terminal model, and check that the
next frame leaves the screen equal to the buffer.

## The loop

`Program::run` owns everything that touches the terminal: the raw-mode guard, a reader thread, the signal handlers and the renderer. The app only sees three methods.

- `event` turns something the terminal reported into a message, or ignores it.
- `update` changes state and returns a `Cmd`. It never touches the terminal.
- `view` draws the state into a `Frame`. It only reads.

The main thread blocks in one place, a channel receiver. The reader thread waits on stdin, the signal pipe and a wake-up pipe together, parses input with `Parser`, and sends events and signals into that channel. Work started by effects sends its messages into the same channel, so nothing wakes the loop by polling on a timer.

When something changes, the loop draws, but no more than `max_fps` times a second. It applies everything that is already waiting before it draws, so a burst of messages costs one frame. Quitting doesn't draw a last frame.

Signals are turned into loop behavior in one place. SIGWINCH resizes the renderer, which repaints, and gives the app an `Event::Resize`. SIGCONT re-enters raw mode and invalidates the renderer. SIGINT, SIGTERM and SIGHUP end the run with an `Interrupted` error that wraps the signal, and the terminal is restored on the way out like on any other exit. Ctrl+C typed in raw mode is not a signal: it reaches the app as a key event, and the app decides whether it quits.

The loop itself is a function over a small `Host` trait (write bytes, report the size, resume raw mode). That is what lets the tests drive it without a terminal, with one test that runs a real `Program` on a pty.

## Effects

`update` returns a `Cmd` and never does the work itself. `Cmd::perform` runs a blocking closure on a worker thread and turns its result into a message; `Cmd::spawn` gives a worker a `Sender` for work that produces many messages, like a token stream; `Cmd::after` delivers a message later; `Cmd::repaint` invalidates the renderer; `Cmd::batch` and `Cmd::quit` do what they say. Apps that have their own threads or a runtime use `Program::sender()`.

The pool is bounded, timers share one thread, and everything that hasn't started is dropped when the program ends. The reasoning and the alternatives are in `docs/adr/0001-executor.md`.


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

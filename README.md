# CrewTUI

A terminal UI framework for Rust built around state, messages, update and view. You write a struct for the state, an enum for the messages, and three methods. CrewTUI owns the terminal, the event loop, input parsing, buffering and diffing, and puts the terminal back the way it found it.

It is not React for the terminal: there is no virtual DOM, no hooks and no component lifecycle. It is not a port of Ratatui or Bubble Tea either. It borrows the drawing model of the first (widgets draw into a buffer) and the update loop of the second, and leaves the rest.

```rust,no_run
use crewtui::{App, Cmd, Event, Frame, KeyCode, Program, Style};

struct Counter(i32);

enum Msg {
    Up,
    Quit,
}

impl App for Counter {
    type Message = Msg;

    fn event(&self, event: Event) -> Option<Msg> {
        match event {
            Event::Key(k) if k.code == KeyCode::Char('+') => Some(Msg::Up),
            Event::Key(k) if k.code == KeyCode::Char('q') => Some(Msg::Quit),
            _ => None,
        }
    }

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Up => {
                self.0 += 1;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let text = format!("count: {}  (+ to add, q to quit)", self.0);
        frame.buffer_mut().set_string(0, 0, &text, Style::new());
    }
}

fn main() -> std::io::Result<()> {
    Program::new(Counter(0)).run().map(|_| ())
}
```

## What is in it

- An event loop with effects: `Cmd::perform` and `Cmd::spawn` run work on a small thread pool and send the result back as a message, `Cmd::after` is a timer, and a `Sender` lets your own threads or an async runtime post messages. No async runtime is required.
- A renderer with two buffers that writes only what changed, and recovers with a full repaint when the terminal and its idea of the screen stop agreeing.
- Layout with fixed, fill, percentage, min and max constraints, plus padding, margin, gap and `Justify` for leftover space.
- Widgets: `Paragraph`, `Block`, `List`, `Input`, `Scrollbar`, `Progress`, `Spinner` and `History`, a scrollable transcript that stays cheap with tens of thousands of entries.
- Keys, mouse, paste, focus and resize as typed events, and Unicode-aware width and wrapping.
- Cleanup of the terminal on normal exit, on an error, on a panic and on SIGINT, SIGTERM and SIGHUP. Ctrl+C typed in raw mode reaches the app as a key, so it quits when the app says so.

## Try it

```sh
cargo run --example agent
cargo run --release --example agent -- --stress
```

`agent` is a fake coding assistant. It exists to put the framework under load: a long scrollable history, streaming answers, tool calls, a spinner, an input line and background events. `--stress` starts it with 20,000 lines of history and sends a request every second; one that arrives while an answer is streaming is dropped.

## Documentation

- [Getting started](docs/getting-started.md): a counter, then a command.
- [Architecture](docs/architecture.md): how an event becomes bytes on the terminal.
- [Decisions](docs/adr/): why the executor, renderer, event model, Unicode handling and widgets look the way they do.
- [Performance](docs/perf.md): benchmark and stress numbers.
- [Testing terminal behavior](docs/testing.md), [dependencies](docs/dependencies.md).
- `cargo doc --open` for the API.

## Status

Version 0.1, not published yet. Unix only: Linux is what it is developed and tested on. There is no Windows backend (#26), no Kitty keyboard protocol (#27), and no `Table` or Markdown widgets (#25, #29). The API can still change before 1.0.

The crate has three dependencies: `libc`, `unicode-width` and `unicode-segmentation`. [Why](docs/dependencies.md).

Minimum supported Rust version: 1.85.

## License

MIT or Apache-2.0, at your option.

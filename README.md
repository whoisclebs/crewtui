# CrewTUI

A terminal UI framework for Rust built around state, messages, update and view. You write a struct for the state, an enum for the messages, and three methods. CrewTUI owns the terminal, the event loop, input parsing, buffering and diffing, and puts the terminal back the way it found it.

It is not React for the terminal: there is no virtual DOM, no hooks and no component lifecycle. It is not a port of Ratatui or Bubble Tea either. It borrows the drawing model of the first (widgets draw into a buffer) and the update loop of the second, and leaves the rest.

```rust,no_run
use crewtui::prelude::*;

struct Counter(i32);

enum Msg {
    Up,
    Quit,
}

impl App for Counter {
    type Message = Msg;

    fn event(&self, event: Event) -> Option<Msg> {
        match event {
            Event::Key(k) if k.is(KeyCode::Char('+')) => Some(Msg::Up),
            Event::Key(k) if k.is(KeyCode::Char('q')) => Some(Msg::Quit),
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
        frame.render_widget(Paragraph::new(text), frame.area());
    }
}

fn main() -> std::io::Result<()> {
    crewtui::run(Counter(0))
}
```

## What is in it

- An event loop with effects: `Cmd::perform` and `Cmd::spawn` run work on a small thread pool and send the result back as a message, `Cmd::after` is a timer, and a `Sender` lets your own threads or an async runtime post messages. No async runtime is required.
- A renderer with two buffers that writes only what changed, and recovers with a full repaint when the terminal and its idea of the screen stop agreeing.
- Layout with fixed, fill, percentage, min and max constraints, plus padding, margin, gap and `Justify` for leftover space.
- Widgets: `Paragraph`, `Block`, `List`, `Table`, `Input`, `Scrollbar`, `Progress`, `Spinner` and `History`, a scrollable transcript that stays cheap with tens of thousands of entries.
- Hyperlinks: `Span::link` makes text an OSC 8 link, checked so a URL can't inject terminal commands.
- Keys, mouse, paste, focus and resize as typed events, with the kitty keyboard protocol (releases, repeats, Super) when you ask for it, and Unicode-aware width and wrapping.
- Cleanup of the terminal on normal exit, on an error, on a panic and on SIGINT, SIGTERM and SIGHUP. Ctrl+C typed in raw mode reaches the app as a key, so it quits when the app says so.

## Markdown, code and diffs

`crates/rich` is a second crate, `crewtui-rich`, that turns Markdown, source code and unified diffs into `Text` with styles and links: `Markdown`, `CodeBlock` with a small syntax highlighter, and `Diff` with line numbers. Each is also a widget. It depends on `crewtui` and nothing else, and is here rather than in the core so the core stays small.

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

Version 0.1, not published yet. It runs on Unix, where Linux is what it is developed and tested on, and on the Windows console (Windows 10 or later, where the console understands virtual terminal sequences). The Windows backend compiles and its portable tests run in CI, but the pty tests that drive a whole program are Unix only, so it has had less testing. The API can still change before 1.0.

The crate has three dependencies on any one platform: `unicode-width`, `unicode-segmentation` and `libc` on Unix or `windows-sys` on Windows. [Why](docs/dependencies.md).

Minimum supported Rust version: 1.85.

## License

MIT or Apache-2.0, at your option.

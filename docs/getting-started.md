# Getting started

Add the crate:

```sh
cargo add crewtui
```

An app is a type that implements `App`. It has three methods, and each one has a single job:

- `event` turns something the terminal reported (a key, a click, a resize) into one of your messages, or ignores it.
- `update` applies a message to your state and returns a `Cmd` saying what should happen next.
- `view` draws the state into a `Frame`. It only reads.

## A counter

```rust,no_run
use crewtui::prelude::*;

struct Counter {
    count: i32,
}

enum Msg {
    Add(i32),
    Quit,
}

impl App for Counter {
    type Message = Msg;

    fn event(&self, event: Event) -> Option<Msg> {
        let Event::Key(key) = event else { return None };
        match key {
            k if k.is(KeyCode::Up) => Some(Msg::Add(1)),
            k if k.is(KeyCode::Down) => Some(Msg::Add(-1)),
            k if k.is(KeyCode::Char('q')) => Some(Msg::Quit),
            _ => None,
        }
    }

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Add(n) => {
                self.count += n;
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.area();
        let block = Block::bordered().title("counter");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(
            Paragraph::new(format!("count: {}  (up/down, q to quit)", self.count)),
            inner,
        );
    }
}

fn main() -> std::io::Result<()> {
    crewtui::run(Counter { count: 0 })
}
```

`crewtui::run` puts the terminal in raw mode on the alternate screen, runs the loop and puts everything back when it returns, including when `update` panics or the process gets SIGINT. `Program::new(app).run()` does the same and hands the app back, for when you want the final state; `Program` also takes options such as `terminal_options` and `max_fps`. The `prelude` has the names an app usually needs.

Messages are yours: a plain enum is enough. `event` gets a shared reference and `view` gets a shared reference too, so the only place state changes is `update`. That is also what makes `view` testable: draw the same state into a `Renderer` and compare.

## Adding a command

`update` never does the work itself. It returns a `Cmd`, and the runtime carries it out. This version loads a number on a worker thread, so the interface stays responsive while it waits:

```rust,no_run
use std::time::Duration;

use crewtui::prelude::*;

struct Loader {
    status: String,
}

enum Msg {
    Load,
    Loaded(u32),
    Quit,
}

impl App for Loader {
    type Message = Msg;

    fn event(&self, event: Event) -> Option<Msg> {
        let Event::Key(key) = event else { return None };
        match key {
            k if k.is(KeyCode::Enter) => Some(Msg::Load),
            k if k.is(KeyCode::Char('q')) => Some(Msg::Quit),
            _ => None,
        }
    }

    fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Load => {
                self.status = "loading…".to_owned();
                // Runs on a worker thread; its return value comes back as a message.
                Cmd::perform(|| {
                    std::thread::sleep(Duration::from_secs(2));
                    Msg::Loaded(42)
                })
            }
            Msg::Loaded(n) => {
                self.status = format!("loaded {n}");
                Cmd::none()
            }
            Msg::Quit => Cmd::quit(),
        }
    }

    fn view(&self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(
            Paragraph::new(format!("{}  (Enter loads, q quits)", self.status)),
            area,
        );
    }
}

fn main() -> std::io::Result<()> {
    crewtui::run(Loader { status: "idle".to_owned() })
}
```

The other commands are `Cmd::spawn` for work that sends many messages (a token stream), `Cmd::after` for timers, `Cmd::repaint`, `Cmd::batch` to return several at once, and `Cmd::quit`. Work that should start with the program goes in `App::init`, which returns a `Cmd` and runs before the first frame. Apps with their own threads or an async runtime call `Program::sender()` before `run()` and post messages from wherever they like. There is no need for tokio, and using it doesn't get in the way.

## Where to go next

- `examples/agent.rs` uses most of the crate: a scrollable `History`, an `Input` with the real terminal cursor, a `Spinner`, a `List`, `Layout`, mouse wheel and resize.
- [Architecture](architecture.md) describes what happens between a key press and the bytes on the terminal.

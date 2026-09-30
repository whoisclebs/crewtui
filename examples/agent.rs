//! A fake coding agent, there to push on the framework rather than to be
//! useful.
//!
//! You type a request and press Enter. A worker thread streams a made-up
//! answer back token by token and makes two fake tool calls on the way, a
//! spinner runs while it works, and a clock thread posts a background note
//! now and then. The history scrolls with PageUp, PageDown and the mouse
//! wheel, and stays where you left it while new text arrives. Ctrl+L
//! repaints the screen and Ctrl+C quits.
//!
//! `cargo run --example agent`
//!
//! `--stress` starts with 20,000 lines of history and a request every
//! second, with tokens arriving as fast as the worker can send them.

use std::io;
use std::thread;
use std::time::Duration;

use crewtui::text::{Line, Span};
use crewtui::widgets::{
    Block, History, HistoryState, Input, InputState, List, ListState, Paragraph, Scrollbar, Spinner,
};
use crewtui::{
    App, Cmd, Color, Constraint, Event, Frame, KeyCode, KeyEvent, KeyModifiers, Layout, MouseKind,
    Program, Rect, Sender, Style, TerminalOptions,
};

/// How fast the fake agent works.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pace {
    /// Wait between tokens.
    pub(crate) token: Duration,
    /// How long a fake tool call takes.
    pub(crate) tool: Duration,
}

impl Pace {
    /// What a person can follow.
    pub(crate) const READABLE: Pace = Pace {
        token: Duration::from_millis(25),
        tool: Duration::from_millis(900),
    };

    /// As fast as the worker can send.
    pub(crate) const INSTANT: Pace = Pace {
        token: Duration::ZERO,
        tool: Duration::ZERO,
    };
}

/// Everything `update` reacts to.
#[derive(Debug)]
pub(crate) enum Msg {
    Key(KeyEvent),
    Paste(String),
    Scroll(i32),
    Resize(u16, u16),
    /// A request, from the input box or from a driver.
    Submit(String),
    Token(String),
    ToolStarted(String),
    ToolFinished(String),
    Finished,
    Spin,
    Clock,
    Repaint,
    Quit,
}

/// What the agent is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Ready,
    /// Waiting for the first token, or between tokens.
    Thinking,
    /// Running a tool, which has run for so many spinner ticks.
    Tool(String),
}

pub(crate) struct Agent {
    pub(crate) history: HistoryState,
    pub(crate) input: InputState,
    pub(crate) status: Status,
    pub(crate) tools: Vec<String>,
    pub(crate) tool_state: ListState,
    spinner: Spinner,
    pace: Pace,
    pub(crate) uptime: u64,
    size: (u16, u16),
}

const SPIN_EVERY: Duration = Duration::from_millis(80);
const SIDE_WIDTH: u16 = 26;

pub(crate) fn dim() -> Style {
    Style::new().fg(Color::BrightBlack)
}

fn label(text: &str, color: Color) -> Span<'static> {
    Span::styled(text.to_owned(), Style::new().fg(color).bold())
}

impl Agent {
    pub(crate) fn new(pace: Pace) -> Self {
        let mut history = HistoryState::new();
        history.push(Line::from(vec![
            label("crewtui ", Color::Cyan),
            Span::styled("agent. Ask for anything; nothing here is real.", dim()),
        ]));
        Agent {
            history,
            input: InputState::new(),
            status: Status::Ready,
            tools: Vec::new(),
            tool_state: ListState::new(),
            spinner: Spinner::dots().style(Style::new().fg(Color::Yellow)),
            pace,
            uptime: 0,
            size: (0, 0),
        }
    }

    /// A transcript that is already long, for the stress run.
    pub(crate) fn with_history(mut self, lines: usize) -> Self {
        for i in 0..lines {
            self.history.push(Line::from(vec![
                Span::styled(format!("{i:>6} "), dim()),
                Span::raw(format!(
                    "earlier output, line {i}, with a few words so that some of it wraps"
                )),
            ]));
        }
        self
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.status != Status::Ready
    }

    fn submit(&mut self, request: &str) -> Cmd<Msg> {
        let request = request.trim();
        if request.is_empty() || self.is_busy() {
            return Cmd::none();
        }
        self.history.push(Line::from(vec![
            label("you ▸ ", Color::Green),
            Span::raw(request.to_owned()),
        ]));
        // Tokens are appended to this entry as they arrive; the empty last
        // span is what they extend, so they don't take the label's style.
        self.history.push(Line::from(vec![
            label("agent ▸ ", Color::Cyan),
            Span::raw(String::new()),
        ]));
        self.status = Status::Thinking;
        let pace = self.pace;
        let request = request.to_owned();
        Cmd::batch([
            Cmd::spawn(move |tx| stream_answer(&tx, &request, pace)),
            Cmd::after(SPIN_EVERY, Msg::Spin),
        ])
    }

    fn key(&mut self, key: KeyEvent) -> Cmd<Msg> {
        match key.code {
            KeyCode::Enter => {
                let request = self.input.text().to_owned();
                if request.trim().is_empty() || self.is_busy() {
                    return Cmd::none();
                }
                self.input.clear();
                self.submit(&request)
            }
            KeyCode::PageUp => {
                self.history.page_up();
                Cmd::none()
            }
            KeyCode::PageDown => {
                self.history.page_down();
                Cmd::none()
            }
            _ => {
                self.input.handle_key(key);
                Cmd::none()
            }
        }
    }
}

/// The answer the fake agent gives, as steps for the worker.
enum Step {
    Say(String),
    Tool(String),
}

fn script(request: &str) -> Vec<Step> {
    vec![
        Step::Say(format!(
            "Let me look into “{request}”. I'll start with the entry point.\n"
        )),
        Step::Tool("read_file src/lib.rs".to_owned()),
        Step::Say(
            "The module list is short and the public types are re-exported from the root.\n\
             Here is the part that matters:\n\n    \
             pub use render::Renderer;\n    \
             pub use runtime::{App, Cmd, Program};\n\n\
             Now I'll run the tests to be sure nothing depends on it.\n"
                .to_owned(),
        ),
        Step::Tool("run_tests".to_owned()),
        Step::Say(
            "All of them pass, so the change is safe. In short: the request needs no code \
             change, and the behavior you're after is already there.\n"
                .to_owned(),
        ),
    ]
}

/// Runs on a worker thread, and stops when the program has.
fn stream_answer(tx: &Sender<Msg>, request: &str, pace: Pace) {
    for step in script(request) {
        match step {
            Step::Say(text) => {
                for token in text.split_inclusive(' ') {
                    if tx.send(Msg::Token(token.to_owned())).is_err() {
                        return;
                    }
                    if !pace.token.is_zero() {
                        thread::sleep(pace.token);
                    }
                }
            }
            Step::Tool(name) => {
                if tx.send(Msg::ToolStarted(name.clone())).is_err() {
                    return;
                }
                if !pace.tool.is_zero() {
                    thread::sleep(pace.tool);
                }
                if tx.send(Msg::ToolFinished(name)).is_err() {
                    return;
                }
            }
        }
    }
    let _ = tx.send(Msg::Finished);
}

impl App for Agent {
    type Message = Msg;

    fn event(&self, event: Event) -> Option<Msg> {
        match event {
            Event::Key(key) if key.modifiers.contains(KeyModifiers::CTRL) => match key.code {
                KeyCode::Char('c') => Some(Msg::Quit),
                KeyCode::Char('l') => Some(Msg::Repaint),
                _ => Some(Msg::Key(key)),
            },
            Event::Key(key) => Some(Msg::Key(key)),
            Event::Paste(text) => Some(Msg::Paste(text)),
            Event::Resize(w, h) => Some(Msg::Resize(w, h)),
            Event::Mouse(m) => match m.kind {
                MouseKind::ScrollUp => Some(Msg::Scroll(-3)),
                MouseKind::ScrollDown => Some(Msg::Scroll(3)),
                _ => None,
            },
            _ => None,
        }
    }

    fn update(&mut self, message: Msg) -> Cmd<Msg> {
        match message {
            Msg::Quit => return Cmd::quit(),
            Msg::Repaint => return Cmd::repaint(),
            Msg::Key(key) => return self.key(key),
            Msg::Paste(text) => self.input.insert_str(&text),
            Msg::Scroll(rows) if rows < 0 => self.history.scroll_up(rows.unsigned_abs() as usize),
            Msg::Scroll(rows) => self.history.scroll_down(rows as usize),
            Msg::Resize(w, h) => self.size = (w, h),
            Msg::Submit(request) => return self.submit(&request),
            Msg::Token(text) => self.history.append(&text),
            Msg::ToolStarted(name) => {
                self.history.push(Line::from(vec![
                    Span::styled("  ⚙ ", Style::new().fg(Color::Yellow)),
                    Span::styled(format!("{name} …"), dim()),
                ]));
                self.history.push(Line::from(vec![
                    label("agent ▸ ", Color::Cyan),
                    Span::raw(String::new()),
                ]));
                self.status = Status::Tool(name);
            }
            Msg::ToolFinished(name) => {
                self.tools.push(format!("✓ {name}"));
                self.tool_state.select(Some(self.tools.len() - 1));
                self.status = Status::Thinking;
            }
            Msg::Finished => self.status = Status::Ready,
            Msg::Spin => {
                if !self.is_busy() {
                    return Cmd::none();
                }
                self.spinner.tick();
                return Cmd::after(SPIN_EVERY, Msg::Spin);
            }
            Msg::Clock => {
                self.uptime += 1;
                // Something that happens while nobody is asking for it.
                if self.uptime % 5 == 0 {
                    self.history.push(Line::from(Span::styled(
                        format!("· background: indexed {} files", 100 + self.uptime),
                        dim(),
                    )));
                    if self.is_busy() {
                        // Tokens append to the last line, so the answer
                        // needs a line of its own to keep going on.
                        self.history.push(Line::from(vec![
                            label("agent ▸ ", Color::Cyan),
                            Span::raw(String::new()),
                        ]));
                    }
                }
            }
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame<'_>) {
        let area = frame.area();
        let [header, body, status, input] = Layout::column()
            .constraints([
                Constraint::Fixed(1),
                Constraint::Fill(1),
                Constraint::Fixed(1),
                Constraint::Fixed(3),
            ])
            .split_array(area);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                label("crewtui agent", Color::Cyan),
                Span::styled(
                    format!("  up {}s  {}x{}", self.uptime, area.width, area.height),
                    dim(),
                ),
            ])),
            header,
        );

        // A side panel only when there is room for it.
        let (main, side) = if body.width >= 3 * SIDE_WIDTH {
            let [main, side] = Layout::row()
                .constraints([Constraint::Fill(1), Constraint::Fixed(SIDE_WIDTH)])
                .split_array(body);
            (main, Some(side))
        } else {
            (body, None)
        };

        let frame_block = Block::bordered().title("history");
        let inner = frame_block.inner(main);
        frame.render_widget(frame_block, main);
        let [text, bar]: [Rect; 2] = Layout::row()
            .constraints([Constraint::Fill(1), Constraint::Fixed(1)])
            .split_array(inner);
        frame.render_stateful_widget(History::new(), text, &self.history);
        frame.render_widget(
            Scrollbar::vertical()
                .content(self.history.content_rows())
                .viewport(self.history.viewport_rows())
                .position(self.history.position()),
            bar,
        );

        if let Some(side) = side {
            let items = if self.tools.is_empty() {
                vec!["no tool calls yet".to_owned()]
            } else {
                self.tools.clone()
            };
            frame.render_stateful_widget(
                List::new(items)
                    .block(Block::bordered().title("tools"))
                    .highlight_style(Style::new().bold()),
                side,
                &self.tool_state,
            );
        }

        self.draw_status(frame, status);

        frame.render_stateful_widget(
            Input::new()
                .placeholder("ask the agent")
                .block(Block::bordered().title("you")),
            input,
            &self.input,
        );
    }
}

impl Agent {
    fn draw_status(&self, frame: &mut Frame<'_>, area: Rect) {
        let (text, style) = match &self.status {
            Status::Ready => (
                "ready · Enter sends · PageUp/PageDown scroll · Ctrl+L repaint · Ctrl+C quit"
                    .to_owned(),
                dim(),
            ),
            Status::Thinking => ("thinking".to_owned(), Style::new()),
            Status::Tool(name) => (format!("running {name}"), Style::new()),
        };
        let mut text_area = area;
        if self.is_busy() && area.width > 2 {
            frame.render_widget(self.spinner, Rect { width: 1, ..area });
            text_area = Rect {
                x: area.x + 2,
                width: area.width - 2,
                ..area
            };
        }
        let mut spans = vec![Span::styled(text, style)];
        if !self.history.is_following() {
            spans.push(Span::styled(
                "  ⇣ scrolled up, PageDown to follow",
                Style::new().fg(Color::Yellow),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), text_area);
    }
}

/// Runs the agent on the terminal until Ctrl+C.
pub(crate) fn run(pace: Pace, stress: bool) -> io::Result<()> {
    let mut agent = Agent::new(pace);
    if stress {
        agent = agent.with_history(20_000);
    }
    let options = TerminalOptions::default().mouse(true);
    let program = Program::new(agent).terminal_options(options);

    // Background events: a clock, and in the stress run a request every
    // second. Both stop by themselves when the program has ended.
    let tx = program.sender();
    thread::spawn(move || {
        let mut n = 0u64;
        loop {
            thread::sleep(Duration::from_secs(1));
            n += 1;
            if tx.send(Msg::Clock).is_err() {
                return;
            }
            if stress && tx.send(Msg::Submit(format!("stress request {n}"))).is_err() {
                return;
            }
        }
    });

    program.run().map(|_| ())
}

fn main() -> io::Result<()> {
    let stress = std::env::args().any(|a| a == "--stress");
    let pace = if stress {
        Pace::INSTANT
    } else {
        Pace::READABLE
    };
    run(pace, stress)
}

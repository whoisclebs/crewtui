//! Testing an [`App`] without a terminal.
//!
//! This is a helper for tests, and it is part of the public API so an app can
//! use it from its own test suite. It needs nothing but the crate: no
//! terminal, no threads and no clock.
//!
//! A [`Harness`] owns the app and a screen size. It does what the run loop
//! does, in one thread and in an order the test controls: it turns events
//! into messages, passes them to `update`, and looks at the commands that
//! come back. Effects that would run elsewhere in a real program, such as
//! [`Cmd::perform`] and [`Cmd::after`], wait until the test asks for them.
//!
//! ```
//! use crewtui::prelude::*;
//! use crewtui::testing::Harness;
//! use crewtui::{Cmd, Event, Frame, KeyCode, KeyEvent, KeyModifiers};
//!
//! struct Counter(i32);
//!
//! enum Msg {
//!     Up,
//!     Copy,
//! }
//!
//! impl App for Counter {
//!     type Message = Msg;
//!
//!     fn event(&self, event: Event) -> Option<Msg> {
//!         match event {
//!             Event::Key(k) if k.is(KeyCode::Char('+')) => Some(Msg::Up),
//!             Event::Key(k) if k.is(KeyCode::Char('c')) => Some(Msg::Copy),
//!             _ => None,
//!         }
//!     }
//!
//!     fn update(&mut self, msg: Msg) -> Cmd<Msg> {
//!         match msg {
//!             Msg::Up => {
//!                 self.0 += 1;
//!                 Cmd::none()
//!             }
//!             Msg::Copy => Cmd::copy_to_clipboard(self.0.to_string()),
//!         }
//!     }
//!
//!     fn view(&self, frame: &mut Frame) {
//!         frame.render_widget(Paragraph::new(format!("count: {}", self.0)), frame.area());
//!     }
//! }
//!
//! let mut harness = Harness::new(Counter(0), 20, 2);
//! harness.start();
//! harness.key(KeyEvent::new(KeyCode::Char('+'), KeyModifiers::NONE));
//! harness.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
//! assert_eq!(harness.screen().rows(), ["count: 1", ""]);
//! assert_eq!(harness.clipboard(), ["1"]);
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use crate::runtime::{Effect, Input, effects_of};
use crate::{App, Buffer, Cell, Cmd, Event, Frame, KeyEvent, Rect, Sender};

/// The most jobs one call to [`Harness::run_commands`] runs, so a program
/// that keeps starting new work fails the test instead of hanging it.
const MAX_JOBS: usize = 10_000;

enum Job<M> {
    Perform(Box<dyn FnOnce() -> M + Send>),
    Spawn(Box<dyn FnOnce(Sender<M>) + Send>),
}

/// An app running without a terminal.
///
/// Events and messages go through the app's own `event` and `update`, as in
/// [`Program`](crate::Program). What the commands ask for is handled like this:
///
/// - [`Cmd::quit`] is remembered; see [`Harness::has_quit`]. The harness
///   keeps accepting input after it, so a test can check what came next.
/// - [`Cmd::copy_to_clipboard`] records the text in [`Harness::clipboard`],
///   for the texts that would really be sent.
/// - [`Cmd::repaint`] does nothing, since the screen is drawn from the state
///   every time it is asked for.
/// - [`Cmd::perform`] and [`Cmd::spawn`] wait in a queue until
///   [`Harness::run_commands`], which runs them on the calling thread.
/// - [`Cmd::after`] waits until [`Harness::fire_timers`], whatever the delay.
///   [`Harness::pending_timers`] lists the delays.
pub struct Harness<A: App> {
    app: A,
    width: u16,
    height: u16,
    jobs: VecDeque<Job<A::Message>>,
    timers: Vec<(Duration, A::Message)>,
    clipboard: Vec<String>,
    quit: bool,
    tx: std::sync::mpsc::Sender<Input<A::Message>>,
    rx: Receiver<Input<A::Message>>,
}

impl<A: App> Harness<A> {
    /// A harness for `app` on a screen of `width` by `height` cells. Nothing
    /// has run yet, not even [`App::init`]; call [`Harness::start`] for that.
    pub fn new(app: A, width: u16, height: u16) -> Self {
        let (tx, rx) = channel();
        Harness {
            app,
            width,
            height,
            jobs: VecDeque::new(),
            timers: Vec::new(),
            clipboard: Vec::new(),
            quit: false,
            tx,
            rx,
        }
    }

    /// The app.
    pub fn app(&self) -> &A {
        &self.app
    }

    /// The app, to set it up or change it behind its own back.
    pub fn app_mut(&mut self) -> &mut A {
        &mut self.app
    }

    /// Takes the app back, ending the test.
    pub fn into_app(self) -> A {
        self.app
    }

    /// Runs the commands [`App::init`] returns, as the program does before
    /// its first frame.
    pub fn start(&mut self) -> &mut Self {
        let cmd = self.app.init();
        self.enqueue(cmd);
        self
    }

    /// Delivers an event: the app's `event` turns it into a message, if it
    /// wants one, and `update` gets that.
    pub fn event(&mut self, event: Event) -> &mut Self {
        if let Some(message) = self.app.event(event) {
            self.message(message);
        }
        self
    }

    /// Delivers a key press. Shorthand for `event(Event::Key(key))`.
    pub fn key(&mut self, key: KeyEvent) -> &mut Self {
        self.event(Event::Key(key))
    }

    /// Passes a message straight to `update`, as if a worker had sent it.
    pub fn message(&mut self, message: A::Message) -> &mut Self {
        let cmd = self.app.update(message);
        self.enqueue(cmd);
        self
    }

    /// Changes the screen size and tells the app with an [`Event::Resize`],
    /// as the program does.
    pub fn resize(&mut self, width: u16, height: u16) -> &mut Self {
        self.width = width;
        self.height = height;
        self.event(Event::Resize(width, height))
    }

    fn enqueue(&mut self, cmd: Cmd<A::Message>) {
        for effect in effects_of(cmd) {
            match effect {
                Effect::Quit => self.quit = true,
                Effect::Repaint => {}
                Effect::Copy(text) => {
                    if crate::clipboard::osc52(&text).is_some() {
                        self.clipboard.push(text);
                    }
                }
                Effect::Perform(work) => self.jobs.push_back(Job::Perform(work)),
                Effect::Spawn(work) => self.jobs.push_back(Job::Spawn(work)),
                Effect::After(delay, message) => self.timers.push((delay, message)),
            }
        }
    }

    /// How many [`Cmd::perform`] and [`Cmd::spawn`] jobs are waiting.
    pub fn pending_jobs(&self) -> usize {
        self.jobs.len()
    }

    /// The delays of the [`Cmd::after`] timers that are waiting, in the order
    /// they were asked for.
    pub fn pending_timers(&self) -> Vec<Duration> {
        self.timers.iter().map(|(delay, _)| *delay).collect()
    }

    /// Runs the waiting [`Cmd::perform`] and [`Cmd::spawn`] jobs on this
    /// thread and gives their messages to `update`, until none are left,
    /// including those that the messages started.
    ///
    /// A [`Cmd::spawn`] job gets a [`Sender`] and runs to its end before this
    /// returns. What it sends goes to `update` in order, but a job that hands
    /// its sender to another thread and waits for it will block the test. A
    /// job that panics fails the test, where in a program it would be
    /// dropped or reported through the `_catching` variants.
    ///
    /// # Panics
    ///
    /// When more than 10,000 jobs run in one call, which means the app keeps
    /// starting work.
    pub fn run_commands(&mut self) -> &mut Self {
        let mut ran = 0;
        while let Some(job) = self.jobs.pop_front() {
            ran += 1;
            assert!(
                ran <= MAX_JOBS,
                "the app keeps starting commands: over {MAX_JOBS} jobs ran"
            );
            match job {
                Job::Perform(work) => {
                    let message = work();
                    self.message(message);
                }
                Job::Spawn(work) => {
                    work(Sender::new(self.tx.clone()));
                    self.deliver_sent();
                }
            }
        }
        self
    }

    fn deliver_sent(&mut self) {
        while let Ok(input) = self.rx.try_recv() {
            if let Input::Message(message) = input {
                self.message(message);
            }
        }
    }

    /// Delivers the message of every waiting [`Cmd::after`] timer, shortest
    /// delay first and in the order asked for among equal ones. A timer that
    /// `update` starts in response waits for the next call.
    pub fn fire_timers(&mut self) -> &mut Self {
        let mut timers = std::mem::take(&mut self.timers);
        timers.sort_by_key(|(delay, _)| *delay);
        for (_, message) in timers {
            self.message(message);
        }
        self
    }

    /// The texts [`Cmd::copy_to_clipboard`] was asked to copy, oldest first.
    /// Text that would not be sent, because it is empty or over
    /// [`CLIPBOARD_LIMIT`](crate::CLIPBOARD_LIMIT), is not listed.
    pub fn clipboard(&self) -> &[String] {
        &self.clipboard
    }

    /// Takes the recorded clipboard texts, leaving the list empty.
    pub fn take_clipboard(&mut self) -> Vec<String> {
        std::mem::take(&mut self.clipboard)
    }

    /// Whether the app has returned [`Cmd::quit`].
    pub fn has_quit(&self) -> bool {
        self.quit
    }

    /// Draws the app with its current state and returns what is on screen.
    /// The state is not changed, so this can be called as often as needed.
    pub fn screen(&self) -> Snapshot {
        let area = Rect::new(0, 0, self.width, self.height);
        let mut buffer = Buffer::new(area);
        let mut frame = Frame::new(&mut buffer);
        self.app.view(&mut frame);
        let cursor = frame.cursor().filter(|&(x, y)| area.contains(x, y));
        Snapshot { buffer, cursor }
    }
}

impl<A: App> fmt::Debug for Harness<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Harness")
            .field("size", &(self.width, self.height))
            .field("pending_jobs", &self.jobs.len())
            .field("pending_timers", &self.timers.len())
            .field("clipboard", &self.clipboard)
            .field("quit", &self.quit)
            .finish_non_exhaustive()
    }
}

/// What one frame of an app looked like: its cells and where the cursor was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    buffer: Buffer,
    cursor: Option<(u16, u16)>,
}

impl Snapshot {
    /// The width in columns.
    pub fn width(&self) -> u16 {
        self.buffer.area().width
    }

    /// The height in rows.
    pub fn height(&self) -> u16 {
        self.buffer.area().height
    }

    /// The text of each row, top to bottom, with trailing spaces removed. A
    /// wide glyph is one character, as on screen.
    pub fn rows(&self) -> Vec<String> {
        let area = self.buffer.area();
        (area.y..area.bottom())
            .map(|y| {
                let row: String = (area.x..area.right())
                    .filter_map(|x| self.buffer.get(x, y))
                    .map(Cell::symbol)
                    .collect();
                row.trim_end().to_owned()
            })
            .collect()
    }

    /// All the rows joined by `\n`, with trailing blank rows removed.
    pub fn text(&self) -> String {
        let mut rows = self.rows();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }

    /// Whether `needle` is in the text of a single row. Text that wrapped
    /// onto two rows is not found across them.
    pub fn contains(&self, needle: &str) -> bool {
        self.rows().iter().any(|row| row.contains(needle))
    }

    /// The cell at column `x` and row `y`, with its symbol and style.
    ///
    /// # Panics
    ///
    /// When the position is outside the screen. [`Snapshot::buffer`] has
    /// `get`, which returns `None` there.
    pub fn cell(&self, x: u16, y: u16) -> &Cell {
        self.buffer.get(x, y).unwrap_or_else(|| {
            panic!(
                "cell ({x}, {y}) is outside the {}x{} screen",
                self.width(),
                self.height()
            )
        })
    }

    /// The hyperlink at column `x` and row `y`, if the text there is one.
    pub fn link_at(&self, x: u16, y: u16) -> Option<&str> {
        self.buffer.link_at(x, y)
    }

    /// Where the app put the terminal cursor, or `None` when it is hidden.
    /// A position outside the screen counts as hidden, as it does when drawing.
    pub fn cursor(&self) -> Option<(u16, u16)> {
        self.cursor
    }

    /// The buffer behind the snapshot.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::{Line, Span};
    use crate::widgets::{InputState, Paragraph};
    use crate::{Color, KeyCode, KeyModifiers, Style};
    use std::time::Duration;

    #[derive(Debug, PartialEq, Eq)]
    enum Msg {
        Key(char),
        Load,
        Loaded(u32),
        Stream,
        Chunk(u32),
        Tick,
        Copy,
        Quit,
        Resized(u16, u16),
    }

    #[derive(Default)]
    struct Demo {
        input: InputState,
        loaded: Vec<u32>,
        ticks: u32,
        size: (u16, u16),
    }

    impl App for Demo {
        type Message = Msg;

        fn event(&self, event: Event) -> Option<Msg> {
            match event {
                Event::Key(k) if k.is(KeyCode::Char('!')) => Some(Msg::Load),
                Event::Key(k) if k.is(KeyCode::Char('~')) => Some(Msg::Stream),
                Event::Key(k) if k.is(KeyCode::Char('c')) => Some(Msg::Copy),
                Event::Key(k) if k.is(KeyCode::Esc) => Some(Msg::Quit),
                Event::Key(KeyEvent {
                    code: KeyCode::Char(c),
                    ..
                }) => Some(Msg::Key(c)),
                Event::Resize(w, h) => Some(Msg::Resized(w, h)),
                _ => None,
            }
        }

        fn update(&mut self, message: Msg) -> Cmd<Msg> {
            match message {
                Msg::Key(c) => {
                    self.input.insert_char(c);
                    Cmd::none()
                }
                Msg::Load => Cmd::batch([
                    Cmd::perform(|| Msg::Loaded(7)),
                    Cmd::after(Duration::from_secs(2), Msg::Tick),
                    Cmd::after(Duration::from_secs(1), Msg::Tick),
                ]),
                Msg::Loaded(n) => {
                    self.loaded.push(n);
                    if n < 9 {
                        Cmd::perform(move || Msg::Loaded(n + 1))
                    } else {
                        Cmd::none()
                    }
                }
                Msg::Stream => Cmd::spawn(|tx| {
                    for i in 0..3 {
                        let _ = tx.send(Msg::Chunk(i));
                    }
                }),
                Msg::Chunk(n) => {
                    self.loaded.push(100 + n);
                    Cmd::none()
                }
                Msg::Tick => {
                    self.ticks += 1;
                    Cmd::none()
                }
                Msg::Copy => Cmd::batch([
                    Cmd::copy_to_clipboard(self.input.text().to_owned()),
                    Cmd::copy_to_clipboard(""),
                ]),
                Msg::Quit => Cmd::quit(),
                Msg::Resized(w, h) => {
                    self.size = (w, h);
                    Cmd::none()
                }
            }
        }

        fn view(&self, frame: &mut Frame<'_>) {
            let area = frame.area();
            let title = Line::from(vec![
                Span::styled("in: ", Style::new().fg(Color::Green)),
                Span::raw(self.input.text().to_owned()),
                Span::raw(" 中").link("https://e.com/x"),
            ]);
            frame.render_widget(Paragraph::new(title), area);
            let rest = Rect::new(0, 1, area.width, area.height.saturating_sub(1));
            frame.render_widget(
                Paragraph::new(format!(
                    "{:?} ticks={} {}x{}",
                    self.loaded, self.ticks, area.width, area.height
                )),
                rest,
            );
            let (x, y) = (4 + self.input.cursor_column() as u16, 0);
            frame.set_cursor(x, y);
        }

        fn init(&self) -> Cmd<Msg> {
            Cmd::perform(|| Msg::Key('i'))
        }
    }

    fn press(c: char) -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
    }

    fn harness() -> Harness<Demo> {
        Harness::new(Demo::default(), 30, 3)
    }

    #[test]
    fn events_go_through_event_and_update_and_the_screen_shows_the_result() {
        let mut h = harness();
        h.event(press('a')).event(press('b'));
        assert_eq!(h.app().input.text(), "ab");
        let screen = h.screen();
        assert_eq!(screen.rows()[0], "in: ab 中");
        assert_eq!(screen.rows().len(), 3);
        assert_eq!(screen.rows()[1], "[] ticks=0 30x3");
        assert!(screen.contains("ticks=0"));
        assert!(!screen.contains("nope"));
        assert_eq!(screen.text(), "in: ab 中\n[] ticks=0 30x3");
        assert_eq!(screen.width(), 30);
        assert_eq!(screen.height(), 3);
    }

    #[test]
    fn an_event_the_app_ignores_changes_nothing() {
        let mut h = harness();
        h.event(Event::FocusGained);
        assert_eq!(h.app().input.text(), "");
        assert_eq!(h.pending_jobs(), 0);
    }

    #[test]
    fn cells_carry_their_styles_and_links_and_the_cursor_is_reported() {
        let mut h = harness();
        h.event(press('a'));
        let screen = h.screen();
        assert_eq!(screen.cell(0, 0).symbol(), "i");
        assert_eq!(screen.cell(0, 0).style().fg, Some(Color::Green));
        assert_eq!(screen.cell(4, 0).style().fg, None);
        assert_eq!(screen.link_at(6, 0), Some("https://e.com/x"));
        assert_eq!(screen.link_at(0, 0), None);
        assert_eq!(screen.cursor(), Some((5, 0)));
    }

    #[test]
    fn a_cursor_outside_the_screen_is_hidden_and_a_cell_outside_panics() {
        let mut h = harness();
        h.event(press('a'));
        h.resize(3, 3);
        assert_eq!(h.screen().cursor(), None);
        let out = std::panic::catch_unwind(|| {
            let h = Harness::new(Demo::default(), 2, 2);
            h.screen().cell(5, 5).symbol().to_owned()
        });
        assert!(out.is_err());
    }

    #[test]
    fn start_queues_what_init_returned_and_run_commands_delivers_it() {
        let mut h = harness();
        assert_eq!(h.pending_jobs(), 0);
        h.start();
        assert_eq!(h.pending_jobs(), 1);
        assert_eq!(h.app().input.text(), "");
        h.run_commands();
        assert_eq!(h.pending_jobs(), 0);
        assert_eq!(h.app().input.text(), "i");
    }

    #[test]
    fn run_commands_also_runs_the_work_that_the_messages_start() {
        let mut h = harness();
        h.event(press('!'));
        assert_eq!(h.pending_jobs(), 1);
        assert!(h.app().loaded.is_empty());
        h.run_commands();
        assert_eq!(h.app().loaded, [7, 8, 9]);
        assert!(h.screen().contains("[7, 8, 9]"));
    }

    #[test]
    fn spawned_work_sends_its_messages_in_order() {
        let mut h = harness();
        h.event(press('~')).run_commands();
        assert_eq!(h.app().loaded, [100, 101, 102]);
    }

    #[test]
    fn timers_wait_for_fire_timers_and_are_listed_by_delay() {
        let mut h = harness();
        h.event(press('!'));
        assert_eq!(
            h.pending_timers(),
            [Duration::from_secs(2), Duration::from_secs(1)]
        );
        assert_eq!(h.app().ticks, 0);
        h.fire_timers();
        assert_eq!(h.app().ticks, 2);
        assert!(h.pending_timers().is_empty());
        assert!(h.screen().contains("ticks=2"));
    }

    #[test]
    fn clipboard_texts_are_recorded_and_the_ones_that_would_not_be_sent_are_not() {
        let mut h = harness();
        h.event(press('h')).event(press('i')).event(press('c'));
        assert_eq!(h.clipboard(), ["hi"]);
        h.event(press('c'));
        assert_eq!(h.take_clipboard(), ["hi", "hi"]);
        assert!(h.clipboard().is_empty());
    }

    #[test]
    fn quit_is_remembered_and_input_still_works_after_it() {
        let mut h = harness();
        assert!(!h.has_quit());
        h.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(h.has_quit());
        h.event(press('x'));
        assert_eq!(h.app().input.text(), "x");
    }

    #[test]
    fn resize_changes_what_view_draws_on_and_tells_the_app() {
        let mut h = harness();
        h.resize(12, 2);
        assert_eq!(h.app().size, (12, 2));
        let screen = h.screen();
        assert_eq!((screen.width(), screen.height()), (12, 2));
        assert_eq!(screen.rows()[1], "[] ticks=0 1");
    }

    #[test]
    fn messages_can_be_sent_straight_and_the_app_taken_back() {
        let mut h = harness();
        h.message(Msg::Loaded(9));
        h.app_mut().ticks = 5;
        let app = h.into_app();
        assert_eq!(app.loaded, [9]);
        assert_eq!(app.ticks, 5);
    }

    #[test]
    fn work_that_never_stops_fails_the_test_instead_of_hanging() {
        struct Forever;
        impl App for Forever {
            type Message = ();
            fn event(&self, _: Event) -> Option<()> {
                None
            }
            fn update(&mut self, _: ()) -> Cmd<()> {
                Cmd::perform(|| ())
            }
            fn view(&self, _: &mut Frame<'_>) {}
        }
        let out = std::panic::catch_unwind(|| {
            let mut h = Harness::new(Forever, 1, 1);
            h.message(()).run_commands();
        });
        assert!(out.is_err());
    }

    #[test]
    fn the_harness_has_a_debug_form_that_does_not_need_the_app_to_have_one() {
        let h = harness();
        let text = format!("{h:?}");
        assert!(text.contains("pending_jobs"), "{text}");
    }
}

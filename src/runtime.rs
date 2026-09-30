//! The application loop: input, update, view, render.

use std::fmt;
use std::io::{self, Write};
use std::marker::PhantomData;
use std::os::fd::RawFd;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::reader::InputReader;
use crate::{Event, Frame, Renderer, Signal, Signals, Terminal, TerminalOptions};

/// An application: state, the messages that change it, and how to draw it.
///
/// The loop is `event` (turn something that happened into a message),
/// `update` (change state, ask for effects), `view` (draw). `update` is the
/// only place state changes, and `view` only reads it, so the same state
/// always draws the same frame.
///
/// ```no_run
/// use crewtui::{App, Cmd, Event, Frame, KeyCode, Program, Style};
///
/// struct Counter(i32);
/// enum Msg { Up, Quit }
///
/// impl App for Counter {
///     type Message = Msg;
///
///     fn event(&self, event: Event) -> Option<Msg> {
///         match event {
///             Event::Key(k) if k.code == KeyCode::Char('+') => Some(Msg::Up),
///             Event::Key(k) if k.code == KeyCode::Char('q') => Some(Msg::Quit),
///             _ => None,
///         }
///     }
///
///     fn update(&mut self, msg: Msg) -> Cmd<Msg> {
///         match msg {
///             Msg::Up => { self.0 += 1; Cmd::none() }
///             Msg::Quit => Cmd::quit(),
///         }
///     }
///
///     fn view(&self, frame: &mut Frame) {
///         let text = format!("count: {}  (+ to add, q to quit)", self.0);
///         frame.buffer_mut().set_string(0, 0, &text, Style::new());
///     }
/// }
///
/// fn main() -> std::io::Result<()> {
///     Program::new(Counter(0)).run().map(|_| ())
/// }
/// ```
pub trait App {
    /// What `update` reacts to.
    type Message: Send + 'static;

    /// Turns something the terminal reported into a message, or ignores it
    /// by returning `None`. There is no default: a default would need
    /// `Message` to be constructible from any [`Event`].
    fn event(&self, event: Event) -> Option<Self::Message>;

    /// Applies a message to the state and says what should happen next.
    /// Never touches the terminal.
    fn update(&mut self, message: Self::Message) -> Cmd<Self::Message>;

    /// Draws the current state.
    fn view(&self, frame: &mut Frame<'_>);
}

/// What `update` asks the runtime to do next.
pub struct Cmd<M> {
    kind: CmdKind,
    _message: PhantomData<fn() -> M>,
}

enum CmdKind {
    None,
    Quit,
}

impl<M> Cmd<M> {
    /// Do nothing.
    pub const fn none() -> Self {
        Cmd {
            kind: CmdKind::None,
            _message: PhantomData,
        }
    }

    /// Stop the program. [`Program::run`] returns the app as it is.
    pub const fn quit() -> Self {
        Cmd {
            kind: CmdKind::Quit,
            _message: PhantomData,
        }
    }
}

impl<M> Default for Cmd<M> {
    fn default() -> Self {
        Cmd::none()
    }
}

impl<M> fmt::Debug for Cmd<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self.kind {
            CmdKind::None => "Cmd::none()",
            CmdKind::Quit => "Cmd::quit()",
        })
    }
}

/// Everything the loop can be woken by.
pub(crate) enum Input<M> {
    Event(Event),
    #[allow(dead_code)] // sent by workers once effects exist
    Message(M),
    Signal(Signal),
    Failed(io::Error),
}

/// The output side of the loop, so it can run against a fake.
pub(crate) trait Host: Write {
    fn size(&self) -> io::Result<(u16, u16)>;
    /// Goes back to raw mode after the process was stopped and continued.
    fn resume(&mut self) -> io::Result<()>;
}

impl Host for Terminal {
    fn size(&self) -> io::Result<(u16, u16)> {
        Terminal::size(self)
    }

    fn resume(&mut self) -> io::Result<()> {
        Terminal::resume(self)
    }
}

/// Runs an [`App`] on the terminal.
///
/// `run` puts the terminal in raw mode, reads input on its own thread,
/// feeds each event through the app, and draws after every batch of
/// changes, at most [`Program::max_fps`] times a second. It returns the app
/// when `update` returns [`Cmd::quit`], and an error if the terminal fails
/// or the process is told to stop. The terminal is restored on every way
/// out, with one exception: a process stopped from outside with SIGTSTP is
/// not restored while it is stopped, and gets its raw mode back on SIGCONT.
pub struct Program<A: App> {
    app: A,
    options: TerminalOptions,
    max_fps: u32,
}

impl<A: App> Program<A> {
    /// A program that runs `app` with the default terminal modes and a
    /// frame rate cap of 60.
    pub fn new(app: A) -> Self {
        Program {
            app,
            options: TerminalOptions::default(),
            max_fps: 60,
        }
    }

    /// Chooses the terminal modes: alternate screen, mouse and so on.
    pub fn terminal_options(mut self, options: TerminalOptions) -> Self {
        self.options = options;
        self
    }

    /// The most frames drawn per second. Changes that arrive faster are
    /// folded into one frame. `0` removes the cap.
    pub fn max_fps(mut self, fps: u32) -> Self {
        self.max_fps = fps;
        self
    }

    /// Runs until the app quits.
    ///
    /// SIGINT, SIGTERM and SIGHUP from outside end the run with an
    /// [`io::ErrorKind::Interrupted`] error that wraps the [`Signal`]. Ctrl+C
    /// typed in raw mode is a key event instead, and the app decides what
    /// it does.
    pub fn run(self) -> io::Result<A> {
        self.run_on(libc::STDIN_FILENO, libc::STDOUT_FILENO, true)
    }

    pub(crate) fn run_on(self, input: RawFd, output: RawFd, signals: bool) -> io::Result<A> {
        // The handlers go in first and come out last, so a signal that
        // arrives while the terminal is being restored is still delivered
        // as a value instead of killing the process with the tty raw.
        let mut held: Option<Signals> = None;
        let signals = if signals {
            Some(Signals::install()?)
        } else {
            None
        };
        let mut terminal = Terminal::enter_on(input, output, self.options)?;
        let (tx, rx) = mpsc::channel();
        let reader = InputReader::spawn(input, signals, tx)?;
        let result = event_loop(self.app, &rx, &mut terminal, self.max_fps);
        // Stop reading, but keep the handlers until `terminal` has dropped.
        held = reader.finish().or(held);
        drop(terminal);
        drop(held);
        result
    }
}

impl<A: App> fmt::Debug for Program<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("options", &self.options)
            .field("max_fps", &self.max_fps)
            .finish_non_exhaustive()
    }
}

/// Handles one input. Returns true when the program should stop.
fn handle<A: App, H: Host>(
    app: &mut A,
    host: &mut H,
    renderer: &mut Renderer,
    dirty: &mut bool,
    input: Input<A::Message>,
) -> io::Result<bool> {
    fn apply<A: App>(app: &mut A, dirty: &mut bool, message: A::Message) -> bool {
        *dirty = true;
        matches!(app.update(message).kind, CmdKind::Quit)
    }
    match input {
        Input::Event(event) => Ok(app.event(event).is_some_and(|m| apply(app, dirty, m))),
        Input::Message(message) => Ok(apply(app, dirty, message)),
        Input::Failed(error) => Err(error),
        Input::Signal(Signal::Resize) => {
            let (width, height) = host.size()?;
            renderer.resize(width, height);
            *dirty = true;
            Ok(app
                .event(Event::Resize(width, height))
                .is_some_and(|m| apply(app, dirty, m)))
        }
        Input::Signal(Signal::Continue) => {
            host.resume()?;
            renderer.invalidate();
            *dirty = true;
            Ok(false)
        }
        Input::Signal(signal) => Err(io::Error::new(io::ErrorKind::Interrupted, signal)),
    }
}

/// Draws once, then processes input until the app quits.
///
/// The loop blocks until something arrives. When a change has been made it
/// draws, but no sooner than `1 / max_fps` after the previous frame, and
/// whatever arrives meanwhile is applied first and shares that frame.
pub(crate) fn event_loop<A: App, H: Host>(
    mut app: A,
    rx: &Receiver<Input<A::Message>>,
    host: &mut H,
    max_fps: u32,
) -> io::Result<A> {
    let (width, height) = host.size()?;
    let mut renderer = Renderer::new(width, height);
    let interval = if max_fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / f64::from(max_fps))
    };
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;
    let disconnected = || io::Error::new(io::ErrorKind::BrokenPipe, "the input thread stopped");

    loop {
        let input = if dirty {
            let wait = last_draw.map_or(Duration::ZERO, |t| interval.saturating_sub(t.elapsed()));
            if wait.is_zero() {
                renderer.present(&mut *host, |buffer| app.view(&mut Frame::new(buffer)))?;
                last_draw = Some(Instant::now());
                dirty = false;
                continue;
            }
            match rx.recv_timeout(wait) {
                Ok(input) => input,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Err(disconnected()),
            }
        } else {
            rx.recv().map_err(|_| disconnected())?
        };
        if handle(&mut app, host, &mut renderer, &mut dirty, input)? {
            return Ok(app);
        }
        // Everything already waiting goes into the same frame.
        while let Ok(input) = rx.try_recv() {
            if handle(&mut app, host, &mut renderer, &mut dirty, input)? {
                return Ok(app);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Pty, Screen, drain_fd, same, write_fd};
    use crate::{KeyCode, KeyEvent, Modifiers, Rect, Style};
    use std::cell::{Cell as StdCell, RefCell};
    use std::collections::VecDeque;
    use std::sync::mpsc::{Sender, channel};
    use std::thread;
    use std::time::Instant;

    struct FakeHost {
        out: Vec<u8>,
        sizes: RefCell<VecDeque<(u16, u16)>>,
        last: (u16, u16),
        resumed: u32,
        fail_writes: bool,
    }

    impl FakeHost {
        fn new(width: u16, height: u16) -> Self {
            FakeHost {
                out: Vec::new(),
                sizes: RefCell::new(VecDeque::new()),
                last: (width, height),
                resumed: 0,
                fail_writes: false,
            }
        }
    }

    impl Write for FakeHost {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if self.fail_writes {
                return Err(io::Error::other("the terminal went away"));
            }
            self.out.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Host for FakeHost {
        fn size(&self) -> io::Result<(u16, u16)> {
            let mut sizes = self.sizes.borrow_mut();
            // Each answer is used up, except the last one.
            let size = if sizes.len() > 1 {
                sizes.pop_front()
            } else {
                sizes.front().copied()
            };
            Ok(size.unwrap_or(self.last))
        }

        fn resume(&mut self) -> io::Result<()> {
            self.resumed += 1;
            Ok(())
        }
    }

    #[derive(Debug, PartialEq)]
    enum Msg {
        Up,
        Ping,
        Quit,
        Resized(u16, u16),
    }

    #[derive(Debug)]
    struct Counter {
        n: i32,
        updates: u32,
        draws: StdCell<u32>,
        area: StdCell<Option<Rect>>,
        resized: Option<(u16, u16)>,
        frames: Option<Sender<()>>,
    }

    impl Counter {
        fn new() -> Self {
            Counter {
                n: 0,
                updates: 0,
                draws: StdCell::new(0),
                area: StdCell::new(None),
                resized: None,
                frames: None,
            }
        }

        /// A counter that reports every call to `view` on the channel.
        fn with_frames() -> (Counter, Receiver<()>) {
            let (tx, rx) = channel();
            let mut app = Counter::new();
            app.frames = Some(tx);
            (app, rx)
        }
    }

    impl App for Counter {
        type Message = Msg;

        fn event(&self, event: Event) -> Option<Msg> {
            match event {
                Event::Key(k) => match k.code {
                    KeyCode::Char('+') => Some(Msg::Up),
                    KeyCode::Char('p') => Some(Msg::Ping),
                    KeyCode::Char('q') => Some(Msg::Quit),
                    _ => None,
                },
                Event::Resize(w, h) => Some(Msg::Resized(w, h)),
                _ => None,
            }
        }

        fn update(&mut self, message: Msg) -> Cmd<Msg> {
            self.updates += 1;
            match message {
                Msg::Up => self.n += 1,
                Msg::Ping => {}
                Msg::Resized(w, h) => self.resized = Some((w, h)),
                Msg::Quit => return Cmd::quit(),
            }
            Cmd::none()
        }

        fn view(&self, frame: &mut Frame<'_>) {
            self.draws.set(self.draws.get() + 1);
            self.area.set(Some(frame.area()));
            let text = format!("count: {}", self.n);
            frame.buffer_mut().set_string(0, 0, &text, Style::new());
            if let Some(frames) = &self.frames {
                let _ = frames.send(());
            }
        }
    }

    fn key(c: char) -> Input<Msg> {
        Input::Event(Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            modifiers: Modifiers::NONE,
        }))
    }

    /// Runs the loop over `inputs`, which must end the program.
    fn run(host: &mut FakeHost, inputs: Vec<Input<Msg>>, max_fps: u32) -> io::Result<Counter> {
        let (tx, rx) = channel();
        for input in inputs {
            tx.send(input).unwrap();
        }
        // With the sender gone, an empty queue means the input thread died.
        drop(tx);
        event_loop(Counter::new(), &rx, host, max_fps)
    }

    fn screen(host: &FakeHost, width: u16, height: u16) -> Screen {
        let mut screen = Screen::new(width, height);
        screen.feed(&host.out);
        screen
    }

    #[test]
    fn events_become_messages_and_the_view_is_drawn() {
        let mut host = FakeHost::new(20, 3);
        let app = run(&mut host, vec![key('+'), key('+'), key('q')], 0).unwrap();
        assert_eq!(app.n, 2);
        assert_eq!(app.updates, 3);
        // The three inputs arrived together, so they share the second frame:
        // the loop stops on `q` before drawing it.
        assert_eq!(app.draws.get(), 1);
        assert_eq!(screen(&host, 20, 3).row(0).trim_end(), "count: 0");
    }

    #[test]
    fn every_change_shows_up_on_screen() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let (app, frames) = Counter::with_frames();
        let handle = thread::spawn(move || {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            for _ in 0..3 {
                tx.send(key('+')).unwrap();
                // Each change gets its own frame before the next one is sent.
                frames.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(app.n, 3);
        assert_eq!(screen(&host, 20, 3).row(0).trim_end(), "count: 3");
    }

    #[test]
    fn a_frame_that_changes_nothing_writes_nothing() {
        let mut quiet = FakeHost::new(20, 3);
        run(&mut quiet, vec![key('q')], 0).unwrap();
        let mut noisy = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let (app, frames) = Counter::with_frames();
        let handle = thread::spawn(move || {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('p')).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &mut noisy, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(app.draws.get(), 2);
        assert_eq!(noisy.out, quiet.out);
    }

    #[test]
    fn events_the_app_does_not_map_never_reach_update() {
        let mut host = FakeHost::new(20, 3);
        let app = run(
            &mut host,
            vec![key('x'), Input::Event(Event::FocusGained), key('q')],
            0,
        )
        .unwrap();
        assert_eq!(app.updates, 1);
    }

    #[test]
    fn a_burst_of_messages_shares_one_frame() {
        let mut host = FakeHost::new(20, 3);
        let mut inputs: Vec<Input<Msg>> = (0..100).map(|_| Input::Message(Msg::Up)).collect();
        inputs.push(key('q'));
        let app = run(&mut host, inputs, 0).unwrap();
        assert_eq!(app.n, 100);
        assert_eq!(app.draws.get(), 1);
    }

    #[test]
    fn the_frame_rate_cap_folds_fast_changes_together() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let handle = thread::spawn(move || {
            for _ in 0..150 {
                tx.send(Input::Message(Msg::Up)).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
            tx.send(key('q')).unwrap();
        });
        let started = Instant::now();
        let app = event_loop(Counter::new(), &rx, &mut host, 20).unwrap();
        handle.join().unwrap();
        assert_eq!(app.n, 150);
        let allowed = (started.elapsed().as_millis() / 50) as u32 + 2;
        assert!(
            app.draws.get() <= allowed,
            "{} draws, at most {allowed} expected",
            app.draws.get()
        );
    }

    #[test]
    fn a_resize_signal_resizes_the_renderer_repaints_and_tells_the_app() {
        let mut host = FakeHost::new(20, 3);
        host.sizes = RefCell::new(VecDeque::from([(20, 3), (30, 6)]));
        let (tx, rx) = channel();
        let (app, frames) = Counter::with_frames();
        let handle = thread::spawn(move || {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Signal(Signal::Resize)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(app.resized, Some((30, 6)));
        assert_eq!(app.area.get(), Some(Rect::new(0, 0, 30, 6)));
        let clears = host.out.windows(4).filter(|w| *w == b"\x1b[2J").count();
        assert_eq!(clears, 2);
        let mut screen = Screen::new(30, 6);
        screen.feed(&host.out);
        assert_eq!(screen.row(0).trim_end(), "count: 0");
    }

    #[test]
    fn a_resize_reaches_update_even_when_the_app_maps_nothing_else() {
        let mut host = FakeHost::new(20, 3);
        host.sizes = RefCell::new(VecDeque::from([(50, 9)]));
        let mut app = Counter::new();
        let mut renderer = Renderer::new(20, 3);
        let mut dirty = false;
        let signal = Input::Signal(Signal::Resize);
        let quit = handle(&mut app, &mut host, &mut renderer, &mut dirty, signal).unwrap();
        assert!(!quit && dirty);
        assert_eq!(app.resized, Some((50, 9)));
        assert_eq!(renderer.area(), Rect::new(0, 0, 50, 9));
    }

    #[test]
    fn stop_signals_end_the_run_with_an_error_that_carries_the_signal() {
        for signal in [Signal::Interrupt, Signal::Terminate, Signal::Hangup] {
            let mut host = FakeHost::new(20, 3);
            let err = run(&mut host, vec![Input::Signal(signal)], 0).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Interrupted);
            let inner = err.get_ref().and_then(|e| e.downcast_ref::<Signal>());
            assert_eq!(inner, Some(&signal));
        }
    }

    #[test]
    fn continuing_after_a_stop_resumes_the_terminal_and_repaints_everything() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let (app, frames) = Counter::with_frames();
        let handle = thread::spawn(move || {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Signal(Signal::Continue)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        event_loop(app, &rx, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(host.resumed, 1);
        let clears = host.out.windows(4).filter(|w| *w == b"\x1b[2J").count();
        assert_eq!(clears, 2, "{:?}", String::from_utf8_lossy(&host.out));
    }

    #[test]
    fn a_failed_read_or_a_lost_input_thread_ends_the_run() {
        let mut host = FakeHost::new(20, 3);
        let err = run(
            &mut host,
            vec![Input::Failed(io::ErrorKind::UnexpectedEof.into())],
            0,
        )
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);

        let mut host = FakeHost::new(20, 3);
        let err = run(&mut host, vec![], 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn a_write_error_ends_the_run() {
        let mut host = FakeHost::new(20, 3);
        host.fail_writes = true;
        let err = run(&mut host, vec![key('q')], 0).unwrap_err();
        assert_eq!(err.to_string(), "the terminal went away");
    }

    #[test]
    fn cmd_debug_names_the_variant() {
        assert_eq!(format!("{:?}", Cmd::<Msg>::none()), "Cmd::none()");
        assert_eq!(format!("{:?}", Cmd::<Msg>::quit()), "Cmd::quit()");
        assert!(matches!(Cmd::<Msg>::default().kind, CmdKind::None));
    }

    fn _assert_sender_is_send(_: Sender<Input<Msg>>) {
        fn is_send<T: Send>() {}
        is_send::<Sender<Input<Msg>>>();
    }

    #[test]
    fn a_program_runs_on_a_real_pty_and_restores_the_terminal() {
        let pty = Pty::open();
        pty.set_size(40, 4);
        let original = pty.termios();
        let master = pty.master;
        let helper = thread::spawn(move || {
            let mut seen = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            let wait_for = |seen: &mut Vec<u8>, needle: &[u8]| {
                while !seen.windows(needle.len()).any(|w| w == needle) {
                    seen.extend(drain_fd(master, 50));
                    assert!(Instant::now() < deadline, "never saw {needle:?}: {seen:?}");
                }
            };
            // The first frame means the terminal is raw. The diff skips the
            // blank cell, so it isn't contiguous text.
            wait_for(&mut seen, b"count:");
            // One key at a time, so each change gets its own frame.
            write_fd(master, b"+");
            wait_for(&mut seen, b"\x1b[1;8H1");
            write_fd(master, b"+");
            wait_for(&mut seen, b"\x1b[1;8H2");
            write_fd(master, b"q");
            wait_for(&mut seen, b"\x1b[?1049l");
            seen
        });
        let app = Program::new(Counter::new())
            .max_fps(0)
            .run_on(pty.slave, pty.slave, false)
            .unwrap();
        let output = helper.join().unwrap();
        assert_eq!(app.n, 2);
        assert!(same(&pty.termios(), &original));
        let mut screen = Screen::new(40, 4);
        screen.feed(&output);
        assert_eq!(screen.row(0).trim_end(), "count: 2");
    }
}

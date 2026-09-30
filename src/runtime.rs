//! The application loop: input, update, view, render.

use std::fmt;
use std::io::{self, Write};
#[cfg(unix)]
use std::os::fd::RawFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::effects::{Effects, Sender};
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
/// use crewtui::prelude::*;
///
/// struct Counter(i32);
/// enum Msg { Up, Quit }
///
/// impl App for Counter {
///     type Message = Msg;
///
///     fn event(&self, event: Event) -> Option<Msg> {
///         match event {
///             Event::Key(k) if k.is(KeyCode::Char('+')) => Some(Msg::Up),
///             Event::Key(k) if k.is(KeyCode::Char('q')) => Some(Msg::Quit),
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
///         frame.render_widget(Paragraph::new(text), frame.area());
///     }
/// }
///
/// fn main() -> std::io::Result<()> {
///     crewtui::run(Counter(0))
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

    /// What to start when the program starts, before the first frame is
    /// drawn: a clock, a first request, a load from disk. It does nothing by
    /// default.
    fn init(&self) -> Cmd<Self::Message> {
        Cmd::none()
    }
}

/// What `update` asks the runtime to do next.
///
/// A `Cmd` describes an effect; the runtime carries it out. Work that
/// blocks, such as a network request or reading a file, goes in
/// [`Cmd::perform`] or [`Cmd::spawn`] and runs on a worker thread. Its
/// result comes back as a message, so state only ever changes in `update`.
///
/// To repeat something on a timer, return [`Cmd::after`] again from the
/// `update` that handles the tick. Nothing needs cancelling: when the
/// program ends, pending timers are dropped, and so are jobs that had not
/// started yet.
pub struct Cmd<M> {
    kind: CmdKind<M>,
}

enum CmdKind<M> {
    None,
    Quit,
    Repaint,
    Copy(String),
    Batch(Vec<Cmd<M>>),
    Perform(Box<dyn FnOnce() -> M + Send>),
    Spawn(Box<dyn FnOnce(Sender<M>) + Send>),
    After(Duration, M),
}

impl<M> Cmd<M> {
    /// Do nothing.
    pub const fn none() -> Self {
        Cmd {
            kind: CmdKind::None,
        }
    }

    /// Stop the program. [`Program::run`] returns the app as it is.
    pub const fn quit() -> Self {
        Cmd {
            kind: CmdKind::Quit,
        }
    }

    /// Clear the screen and draw everything again, for when something
    /// outside the program may have scribbled on the terminal.
    pub const fn repaint() -> Self {
        Cmd {
            kind: CmdKind::Repaint,
        }
    }

    /// Puts `text` on the system clipboard, with an OSC 52 sequence written
    /// to the terminal. It works over SSH too, in terminals that allow it;
    /// those that don't ignore it, and nothing tells the app either way.
    ///
    /// The text goes out as base64, so nothing in it can end the sequence and
    /// run as terminal commands. Text longer than
    /// [`CLIPBOARD_LIMIT`](crate::CLIPBOARD_LIMIT) bytes is not sent at all,
    /// since terminals drop or cut large payloads. The sequence is written
    /// when the command runs, before the next frame.
    pub fn copy_to_clipboard(text: impl Into<String>) -> Self {
        Cmd {
            kind: CmdKind::Copy(text.into()),
        }
    }

    /// Run several commands. They start in the order given.
    pub fn batch(cmds: impl IntoIterator<Item = Cmd<M>>) -> Self {
        Cmd {
            kind: CmdKind::Batch(cmds.into_iter().collect()),
        }
    }
}

impl<M: Send + 'static> Cmd<M> {
    /// Runs `work` on a worker thread and sends its result to `update`.
    ///
    /// At most eight jobs run at once; the rest wait their turn, so a job
    /// that blocks for a long time occupies one of those slots. A job that
    /// panics produces no message; see [`Cmd::perform_catching`] for one that
    /// does.
    pub fn perform(work: impl FnOnce() -> M + Send + 'static) -> Self {
        Cmd {
            kind: CmdKind::Perform(Box::new(work)),
        }
    }

    /// Runs `work` on a worker thread with a [`Sender`], for work that
    /// produces many messages, such as a stream of tokens. `send` starts
    /// returning [`Closed`](crate::Closed) once the program has ended, which
    /// is the signal to stop. It takes one of the same eight slots as
    /// [`Cmd::perform`].
    pub fn spawn(work: impl FnOnce(Sender<M>) + Send + 'static) -> Self {
        Cmd {
            kind: CmdKind::Spawn(Box::new(work)),
        }
    }

    /// Like [`Cmd::perform`], but a panic in `work` becomes a message too:
    /// `on_panic` gets the text of the panic and returns what `update` sees.
    /// Without it a job that panics leaves the app waiting for a reply that
    /// never comes.
    ///
    /// The default panic hook still prints the panic message where the app
    /// is drawn. Use `on_panic` to show it properly.
    pub fn perform_catching(
        work: impl FnOnce() -> M + Send + 'static,
        on_panic: impl FnOnce(String) -> M + Send + 'static,
    ) -> Self {
        Cmd::perform(move || match catch_unwind(AssertUnwindSafe(work)) {
            Ok(message) => message,
            Err(payload) => on_panic(panic_text(payload.as_ref())),
        })
    }

    /// Like [`Cmd::spawn`], but a panic in `work` is sent to `update` as the
    /// message `on_panic` makes from its text. Messages sent before the
    /// panic have arrived as usual.
    pub fn spawn_catching(
        work: impl FnOnce(Sender<M>) + Send + 'static,
        on_panic: impl FnOnce(String) -> M + Send + 'static,
    ) -> Self {
        Cmd::spawn(move |tx| {
            let report = tx.clone();
            if let Err(payload) = catch_unwind(AssertUnwindSafe(move || work(tx))) {
                let _ = report.send(on_panic(panic_text(payload.as_ref())));
            }
        })
    }

    /// Sends `message` to `update` after `delay`.
    pub fn after(delay: Duration, message: M) -> Self {
        Cmd {
            kind: CmdKind::After(delay, message),
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
        match &self.kind {
            CmdKind::None => f.write_str("Cmd::none()"),
            CmdKind::Quit => f.write_str("Cmd::quit()"),
            CmdKind::Repaint => f.write_str("Cmd::repaint()"),
            CmdKind::Copy(text) => write!(f, "Cmd::copy_to_clipboard({} bytes)", text.len()),
            CmdKind::Batch(cmds) => write!(f, "Cmd::batch({} commands)", cmds.len()),
            CmdKind::Perform(_) => f.write_str("Cmd::perform(..)"),
            CmdKind::Spawn(_) => f.write_str("Cmd::spawn(..)"),
            CmdKind::After(delay, _) => write!(f, "Cmd::after({delay:?}, ..)"),
        }
    }
}

/// What running a command asks of the loop itself.
#[derive(Default)]
struct Requests {
    quit: bool,
    repaint: bool,
    /// The texts to copy, in the order asked.
    copies: Vec<String>,
}

impl Requests {
    /// Writes the clipboard sequences, so they reach the terminal in the
    /// order the commands were given and before the next frame.
    fn write_copies(&mut self, host: &mut impl Write) -> io::Result<()> {
        let mut wrote = false;
        for text in self.copies.drain(..) {
            if let Some(sequence) = crate::clipboard::osc52(&text) {
                host.write_all(&sequence)?;
                wrote = true;
            }
        }
        if wrote {
            host.flush()?;
        }
        Ok(())
    }
}

/// Starts the effects in `cmd`, in order, and reports what the loop must do.
fn run_cmd<M: Send + 'static>(cmd: Cmd<M>, effects: &Effects<M>) -> Requests {
    let mut requests = Requests::default();
    // An explicit stack, so a deeply nested batch can't overflow the call
    // stack.
    let mut pending = vec![cmd];
    while let Some(cmd) = pending.pop() {
        match cmd.kind {
            CmdKind::None => {}
            CmdKind::Quit => requests.quit = true,
            CmdKind::Repaint => requests.repaint = true,
            CmdKind::Copy(text) => requests.copies.push(text),
            CmdKind::Batch(cmds) => pending.extend(cmds.into_iter().rev()),
            CmdKind::Perform(work) => effects.perform(work),
            CmdKind::Spawn(work) => effects.spawn(work),
            CmdKind::After(delay, message) => effects.after(delay, message),
        }
    }
    requests
}

/// The text of a panic payload: what `panic!` was given, when it was a string.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a job panicked".to_owned()
    }
}

/// Everything the loop can be woken by.
pub(crate) enum Input<M> {
    Event(Event),
    Message(M),
    Signal(Signal),
    Failed(io::Error),
}

/// The output side of the loop, so it can run against a fake.
pub(crate) trait Host: Write {
    fn size(&self) -> io::Result<(u16, u16)>;
    /// Goes back to raw mode after the process was stopped and continued.
    /// Returns whether it did, which is a reason to repaint. It doesn't when
    /// [`Host::suspend`] just did.
    fn resume(&mut self) -> io::Result<bool>;
    /// Gives the terminal back, stops the process until it is continued, and
    /// takes the terminal again.
    fn suspend(&mut self) -> io::Result<()>;
    /// Whether the terminal was left with the cursor hidden. A fake host
    /// says yes, like a terminal entered with the default options.
    fn cursor_hidden(&self) -> bool {
        true
    }
}

impl Host for Terminal {
    fn size(&self) -> io::Result<(u16, u16)> {
        Terminal::size(self)
    }

    fn resume(&mut self) -> io::Result<bool> {
        if std::mem::take(&mut self.skip_continue) {
            return Ok(false);
        }
        Terminal::resume(self)?;
        Ok(true)
    }

    #[cfg(unix)]
    fn suspend(&mut self) -> io::Result<()> {
        self.restore()?;
        let continued = Signals::stop_process()?;
        Terminal::resume(self)?;
        // The continue signal that woke the process reaches the loop next.
        // It has nothing left to do.
        self.skip_continue = continued;
        Ok(())
    }

    /// The console has no job control, so nothing asks for this.
    #[cfg(windows)]
    fn suspend(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn cursor_hidden(&self) -> bool {
        Terminal::cursor_hidden(self)
    }
}

/// Runs `app` on the terminal until it quits, with the default options.
///
/// This is `Program::new(app).run()` for an app that has no use for its
/// final state, which is most of them:
///
/// ```no_run
/// # use crewtui::{App, Cmd, Event, Frame};
/// # struct MyApp;
/// # impl App for MyApp {
/// #     type Message = ();
/// #     fn event(&self, _: Event) -> Option<()> { None }
/// #     fn update(&mut self, _: ()) -> Cmd<()> { Cmd::none() }
/// #     fn view(&self, _: &mut Frame) {}
/// # }
/// fn main() -> std::io::Result<()> {
///     crewtui::run(MyApp)
/// }
/// ```
pub fn run<A: App>(app: A) -> io::Result<()> {
    Program::new(app).run().map(|_| ())
}

/// Runs an [`App`] on the terminal.
///
/// `run` puts the terminal in raw mode, reads input on its own thread,
/// feeds each event through the app, and draws after every batch of
/// changes, at most [`Program::max_fps`] times a second. It returns the app
/// when `update` returns [`Cmd::quit`], and an error if the terminal fails
/// or the process is told to end. The terminal is restored on every way
/// out. A process stopped from outside with SIGTSTP gives the terminal back
/// before it stops, so the shell is usable, and takes it again when it is
/// continued.
pub struct Program<A: App> {
    app: A,
    options: TerminalOptions,
    max_fps: u32,
    tx: mpsc::Sender<Input<A::Message>>,
    rx: Receiver<Input<A::Message>>,
}

impl<A: App> Program<A> {
    /// A program that runs `app` with the default terminal modes and a
    /// frame rate cap of 60.
    pub fn new(app: A) -> Self {
        let (tx, rx) = mpsc::channel();
        Program {
            app,
            options: TerminalOptions::default(),
            max_fps: 60,
            tx,
            rx,
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

    /// A handle that sends messages to this program from any thread. It can
    /// be taken before [`Program::run`]; what it sends is queued until the
    /// loop starts.
    pub fn sender(&self) -> Sender<A::Message> {
        Sender::new(self.tx.clone())
    }

    /// Runs until the app quits.
    ///
    /// SIGINT, SIGTERM and SIGHUP from outside end the run with an
    /// [`io::ErrorKind::Interrupted`] error that wraps the [`Signal`]. Ctrl+C
    /// typed in raw mode is a key event instead, and the app decides what
    /// it does.
    ///
    #[cfg(unix)]
    pub fn run(self) -> io::Result<A> {
        self.run_on(libc::STDIN_FILENO, libc::STDOUT_FILENO, true)
    }

    /// Runs until the app quits.
    ///
    /// Ctrl+Break and the console window being closed from outside end the
    /// run with an [`io::ErrorKind::Interrupted`] error that wraps the
    /// [`Signal`]. Logging off and shutting down do too, when Windows
    /// delivers them, which it does only to services. Ctrl+C typed in raw mode is a key
    /// event instead, and the app decides what it does.
    #[cfg(windows)]
    pub fn run(self) -> io::Result<A> {
        // The handler goes in first and comes out last, like on Unix. Locals
        // drop in the opposite order to how they are declared, on a panic as
        // well, so the console is restored before the handler is removed.
        let _signals = Signals::install()?;
        let mut terminal = Terminal::enter(self.options)?;
        let reader = InputReader::spawn(
            terminal.input_handle(),
            terminal.output_handle(),
            Some(crate::signals::event()?),
            self.tx.clone(),
            self.options.keyboard_enhancement,
        )?;
        let effects = Effects::new(self.tx);
        let result = event_loop(self.app, &self.rx, &effects, &mut terminal, self.max_fps);
        drop(reader);
        drop(effects);
        drop(terminal);
        result
    }

    #[cfg(unix)]
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
        let reader = InputReader::spawn(
            input,
            signals,
            self.tx.clone(),
            self.options.keyboard_enhancement,
        )?;
        let effects = Effects::new(self.tx);
        let result = event_loop(self.app, &self.rx, &effects, &mut terminal, self.max_fps);
        // Stop reading, but keep the handlers until `terminal` has dropped.
        held = reader.finish().or(held);
        drop(effects);
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
    effects: &Effects<A::Message>,
    host: &mut H,
    renderer: &mut Renderer,
    dirty: &mut bool,
    input: Input<A::Message>,
) -> io::Result<bool> {
    fn apply<A: App, H: Host>(
        app: &mut A,
        effects: &Effects<A::Message>,
        host: &mut H,
        renderer: &mut Renderer,
        dirty: &mut bool,
        message: A::Message,
    ) -> io::Result<bool> {
        *dirty = true;
        let mut requests = run_cmd(app.update(message), effects);
        if requests.repaint {
            renderer.invalidate();
        }
        requests.write_copies(host)?;
        Ok(requests.quit)
    }
    match input {
        Input::Event(event) => match app.event(event) {
            Some(m) => apply(app, effects, host, renderer, dirty, m),
            None => Ok(false),
        },
        Input::Message(message) => apply(app, effects, host, renderer, dirty, message),
        Input::Failed(error) => Err(error),
        Input::Signal(Signal::Resize) => {
            let (width, height) = host.size()?;
            renderer.resize(width, height);
            *dirty = true;
            match app.event(Event::Resize(width, height)) {
                Some(m) => apply(app, effects, host, renderer, dirty, m),
                None => Ok(false),
            }
        }
        Input::Signal(Signal::Continue) => {
            if host.resume()? {
                renderer.invalidate();
                // The cursor may be back to whatever the terminal defaults to.
                renderer.cursor_was_reset();
                *dirty = true;
            }
            Ok(false)
        }
        Input::Signal(Signal::Suspend) => {
            host.suspend()?;
            renderer.invalidate();
            renderer.cursor_was_reset();
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
    effects: &Effects<A::Message>,
    host: &mut H,
    max_fps: u32,
) -> io::Result<A> {
    let (width, height) = host.size()?;
    let mut renderer = Renderer::new(width, height);
    if !host.cursor_hidden() {
        // The renderer assumes a hidden cursor until told otherwise.
        renderer.cursor_was_reset();
    }
    let interval = if max_fps == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs_f64(1.0 / f64::from(max_fps))
    };
    let mut started = run_cmd(app.init(), effects);
    started.write_copies(host)?;
    if started.quit {
        return Ok(app);
    }
    if started.repaint {
        renderer.invalidate();
    }
    let mut last_draw: Option<Instant> = None;
    let mut dirty = true;
    let disconnected = || io::Error::new(io::ErrorKind::BrokenPipe, "the input thread stopped");

    loop {
        let input = if dirty {
            let wait = last_draw.map_or(Duration::ZERO, |t| interval.saturating_sub(t.elapsed()));
            if wait.is_zero() {
                renderer.present(&mut *host, |frame| app.view(frame))?;
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
        if handle(&mut app, effects, host, &mut renderer, &mut dirty, input)? {
            return Ok(app);
        }
        // Everything already waiting goes into the same frame.
        while let Ok(input) = rx.try_recv() {
            if handle(&mut app, effects, host, &mut renderer, &mut dirty, input)? {
                return Ok(app);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Screen;
    #[cfg(unix)]
    use crate::testing::{Pty, drain_fd, same, write_fd};
    use crate::{KeyCode, KeyEvent, KeyModifiers, Rect, Style};
    use std::cell::{Cell as StdCell, RefCell};
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Sender as StdSender, channel};
    use std::thread;
    use std::time::Instant;

    struct FakeHost {
        out: Vec<u8>,
        sizes: RefCell<VecDeque<(u16, u16)>>,
        last: (u16, u16),
        resumed: u32,
        suspended: u32,
        fail_writes: bool,
        cursor_left_visible: bool,
    }

    impl FakeHost {
        fn new(width: u16, height: u16) -> Self {
            FakeHost {
                out: Vec::new(),
                sizes: RefCell::new(VecDeque::new()),
                last: (width, height),
                resumed: 0,
                suspended: 0,
                fail_writes: false,
                cursor_left_visible: false,
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

        fn resume(&mut self) -> io::Result<bool> {
            self.resumed += 1;
            Ok(true)
        }

        fn suspend(&mut self) -> io::Result<()> {
            self.suspended += 1;
            Ok(())
        }

        fn cursor_hidden(&self) -> bool {
            !self.cursor_left_visible
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
        frames: Option<StdSender<()>>,
        on_init: Option<fn() -> Cmd<Msg>>,
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
                on_init: None,
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

        fn init(&self) -> Cmd<Msg> {
            self.on_init.map_or_else(Cmd::none, |f| f())
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

    /// Runs a test's driver thread. If it panics, the loop gets a failure so
    /// the test fails instead of waiting forever for input that won't come.
    fn spawn_driver<M: Send + 'static>(
        tx: StdSender<Input<M>>,
        body: impl FnOnce(&StdSender<Input<M>>) + Send + 'static,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(&tx)));
            if let Err(panic) = outcome {
                let _ = tx.send(Input::Failed(io::Error::other("a test driver panicked")));
                std::panic::resume_unwind(panic);
            }
        })
    }

    fn key(c: char) -> Input<Msg> {
        Input::Event(Event::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    /// Runs the loop over `inputs`, which must end the program.
    fn run(host: &mut FakeHost, inputs: Vec<Input<Msg>>, max_fps: u32) -> io::Result<Counter> {
        let (tx, rx) = channel();
        for input in inputs {
            tx.send(input).unwrap();
        }
        let effects = Effects::new(tx);
        event_loop(Counter::new(), &rx, &effects, host, max_fps)
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
        let effects = Effects::new(tx.clone());
        let (app, frames) = Counter::with_frames();
        let handle = spawn_driver(tx.clone(), move |tx| {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            for _ in 0..3 {
                tx.send(key('+')).unwrap();
                // Each change gets its own frame before the next one is sent.
                frames.recv_timeout(Duration::from_secs(10)).unwrap();
            }
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &effects, &mut host, 0).unwrap();
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
        let effects = Effects::new(tx.clone());
        let (app, frames) = Counter::with_frames();
        let handle = spawn_driver(tx.clone(), move |tx| {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('p')).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &effects, &mut noisy, 0).unwrap();
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
    fn init_starts_work_before_the_first_frame_and_its_result_reaches_update() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx.clone());
        let (mut app, frames) = Counter::with_frames();
        app.on_init = Some(|| Cmd::perform(|| Msg::Up));
        let handle = spawn_driver(tx.clone(), move |tx| {
            // The first frame, then the one after the message from init.
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(app.n, 1);
    }

    #[test]
    fn a_quit_from_init_ends_the_run_without_drawing() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx);
        let mut app = Counter::new();
        app.on_init = Some(Cmd::quit);
        let app = event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        assert_eq!(app.draws.get(), 0);
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
        let effects = Effects::new(tx.clone());
        let handle = spawn_driver(tx.clone(), move |tx| {
            for _ in 0..150 {
                tx.send(Input::Message(Msg::Up)).unwrap();
                thread::sleep(Duration::from_millis(1));
            }
            tx.send(key('q')).unwrap();
        });
        let started = Instant::now();
        let app = event_loop(Counter::new(), &rx, &effects, &mut host, 20).unwrap();
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
        let effects = Effects::new(tx.clone());
        let (app, frames) = Counter::with_frames();
        let handle = spawn_driver(tx.clone(), move |tx| {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Signal(Signal::Resize)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        let app = event_loop(app, &rx, &effects, &mut host, 0).unwrap();
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
        let (tx, _rx) = channel();
        let effects = Effects::new(tx);
        let quit = handle(
            &mut app,
            &effects,
            &mut host,
            &mut renderer,
            &mut dirty,
            signal,
        )
        .unwrap();
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
    fn a_stop_signal_from_outside_suspends_the_host_and_repaints_when_it_returns() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx.clone());
        let (app, frames) = Counter::with_frames();
        let handle = spawn_driver(tx.clone(), move |tx| {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Signal(Signal::Suspend)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(host.suspended, 1);
        let clears = host.out.windows(4).filter(|w| *w == b"\x1b[2J").count();
        assert_eq!(clears, 2, "{:?}", String::from_utf8_lossy(&host.out));
    }

    #[test]
    fn continuing_after_a_stop_resumes_the_terminal_and_repaints_everything() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx.clone());
        let (app, frames) = Counter::with_frames();
        let handle = spawn_driver(tx.clone(), move |tx| {
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Signal(Signal::Continue)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(key('q')).unwrap();
        });
        event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        handle.join().unwrap();
        assert_eq!(host.resumed, 1);
        let clears = host.out.windows(4).filter(|w| *w == b"\x1b[2J").count();
        assert_eq!(clears, 2, "{:?}", String::from_utf8_lossy(&host.out));
    }

    #[test]
    fn a_terminal_entered_with_a_visible_cursor_gets_it_hidden_by_the_first_frame() {
        let mut host = FakeHost::new(20, 3);
        host.cursor_left_visible = true;
        run(&mut host, vec![key('q')], 0).unwrap();
        let hides = host.out.windows(6).filter(|w| *w == b"\x1b[?25l").count();
        assert_eq!(hides, 1, "{:?}", String::from_utf8_lossy(&host.out));

        let mut host = FakeHost::new(20, 3);
        run(&mut host, vec![key('q')], 0).unwrap();
        assert!(!host.out.windows(6).any(|w| w == b"\x1b[?25l"));
    }

    #[test]
    fn a_failed_read_ends_the_run() {
        let mut host = FakeHost::new(20, 3);
        let failure = Input::Failed(io::ErrorKind::UnexpectedEof.into());
        let err = run(&mut host, vec![failure], 0).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
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

    #[derive(Debug, Clone, PartialEq)]
    enum Fx {
        Start,
        Got(u32),
        Done,
        Failed(String),
    }

    type Script = Box<dyn FnMut(&Fx) -> Cmd<Fx>>;

    /// An app whose `update` is a closure, so each test scripts the effects
    /// it wants.
    struct Scripted {
        log: Vec<Fx>,
        script: Script,
        frames: Option<StdSender<()>>,
    }

    impl Scripted {
        fn new(script: impl FnMut(&Fx) -> Cmd<Fx> + 'static) -> Self {
            Scripted {
                log: Vec::new(),
                script: Box::new(script),
                frames: None,
            }
        }
    }

    impl App for Scripted {
        type Message = Fx;

        fn event(&self, _: Event) -> Option<Fx> {
            None
        }

        fn update(&mut self, message: Fx) -> Cmd<Fx> {
            let cmd = (self.script)(&message);
            self.log.push(message);
            cmd
        }

        fn view(&self, _: &mut Frame<'_>) {
            if let Some(frames) = &self.frames {
                let _ = frames.send(());
            }
        }
    }

    /// Runs `app` after sending `Start`, until its script quits.
    fn drive(app: Scripted, host: &mut FakeHost) -> Scripted {
        let (tx, rx) = channel();
        tx.send(Input::Message(Fx::Start)).unwrap();
        let effects = Effects::new(tx);
        event_loop(app, &rx, &effects, host, 0).unwrap()
    }

    #[test]
    fn perform_catching_turns_a_panic_into_a_message_and_passes_a_result_through() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([
                Cmd::perform_catching(|| panic!("disk on fire"), Fx::Failed),
                Cmd::perform_catching(|| Fx::Got(5), Fx::Failed),
                Cmd::perform_catching(
                    || std::panic::panic_any(42_u8),
                    |text| Fx::Failed(format!("other: {text}")),
                ),
            ]),
            Fx::Failed(_) | Fx::Got(_) => Cmd::none(),
            Fx::Done => Cmd::quit(),
        });
        let (tx, rx) = channel();
        tx.send(Input::Message(Fx::Start)).unwrap();
        let effects = Effects::new(tx.clone());
        // Quits once all three answered.
        let handle = spawn_driver(tx.clone(), move |tx| {
            thread::sleep(Duration::from_millis(300));
            tx.send(Input::Message(Fx::Done)).unwrap();
        });
        let app = event_loop(app, &rx, &effects, &mut FakeHost::new(20, 3), 0).unwrap();
        handle.join().unwrap();
        let mut log: Vec<String> = app.log.iter().map(|m| format!("{m:?}")).collect();
        log.sort();
        assert_eq!(
            log,
            [
                r#"Done"#,
                r#"Failed("disk on fire")"#,
                r#"Failed("other: a job panicked")"#,
                r#"Got(5)"#,
                r#"Start"#,
            ]
        );
    }

    #[test]
    fn spawn_catching_delivers_what_was_sent_before_the_panic_and_then_the_failure() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::spawn_catching(
                |tx| {
                    tx.send(Fx::Got(1)).unwrap();
                    tx.send(Fx::Got(2)).unwrap();
                    panic!("stream broke");
                },
                Fx::Failed,
            ),
            Fx::Failed(_) => Cmd::quit(),
            Fx::Got(_) | Fx::Done => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(
            app.log,
            vec![
                Fx::Start,
                Fx::Got(1),
                Fx::Got(2),
                Fx::Failed("stream broke".to_owned())
            ]
        );
    }

    #[test]
    fn perform_runs_blocking_work_off_the_loop_and_returns_its_result_as_a_message() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::perform(|| {
                thread::sleep(Duration::from_millis(20));
                Fx::Got(7)
            }),
            Fx::Got(_) => Cmd::quit(),
            Fx::Done | Fx::Failed(_) => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log, vec![Fx::Start, Fx::Got(7)]);
    }

    #[test]
    fn batch_starts_every_command_and_all_results_arrive() {
        let mut seen = 0;
        let app = Scripted::new(move |m| match m {
            Fx::Start => Cmd::batch([
                Cmd::perform(|| Fx::Got(1)),
                Cmd::perform(|| Fx::Got(2)),
                Cmd::batch([Cmd::perform(|| Fx::Got(3))]),
            ]),
            Fx::Got(_) => {
                seen += 1;
                if seen == 3 { Cmd::quit() } else { Cmd::none() }
            }
            Fx::Done | Fx::Failed(_) => Cmd::none(),
        });
        let mut app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log.remove(0), Fx::Start);
        app.log.sort_by_key(|f| format!("{f:?}"));
        assert_eq!(app.log, vec![Fx::Got(1), Fx::Got(2), Fx::Got(3)]);
    }

    #[test]
    fn a_batch_containing_quit_stops_the_program() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([Cmd::quit(), Cmd::perform(|| Fx::Done)]),
            _ => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log, vec![Fx::Start]);
    }

    #[test]
    fn jobs_that_have_not_started_when_the_program_ends_never_run() {
        // Eight gated jobs fill the pool, so the ninth waits in the queue.
        let gate = Arc::new(AtomicBool::new(false));
        let ran = Arc::new(AtomicBool::new(false));
        let (g, r) = (gate.clone(), ran.clone());
        let app = Scripted::new(move |m| match m {
            Fx::Start => {
                let mut cmds: Vec<Cmd<Fx>> = (0..crate::effects::MAX_WORKERS)
                    .map(|_| {
                        let g = g.clone();
                        Cmd::spawn(move |_| {
                            while !g.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(1));
                            }
                        })
                    })
                    .collect();
                let r = r.clone();
                cmds.push(Cmd::perform(move || {
                    r.store(true, Ordering::SeqCst);
                    Fx::Done
                }));
                cmds.push(Cmd::quit());
                Cmd::batch(cmds)
            }
            _ => Cmd::none(),
        });
        drive(app, &mut FakeHost::new(20, 3));
        gate.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        assert!(
            !ran.load(Ordering::SeqCst),
            "a queued job ran after the program ended"
        );
    }

    #[test]
    fn after_delivers_in_deadline_order_and_not_early() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([
                Cmd::after(Duration::from_millis(80), Fx::Got(2)),
                Cmd::after(Duration::from_millis(20), Fx::Got(1)),
                Cmd::after(Duration::from_millis(140), Fx::Done),
            ]),
            Fx::Done | Fx::Failed(_) => Cmd::quit(),
            Fx::Got(_) => Cmd::none(),
        });
        let started = Instant::now();
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log, vec![Fx::Start, Fx::Got(1), Fx::Got(2), Fx::Done]);
        assert!(started.elapsed() >= Duration::from_millis(140));
    }

    #[test]
    fn a_tick_that_returns_after_again_repeats_until_the_app_stops() {
        let mut ticks = 0;
        let app = Scripted::new(move |m| match m {
            Fx::Start | Fx::Got(_) => {
                ticks += 1;
                if ticks < 5 {
                    Cmd::after(Duration::from_millis(5), Fx::Got(ticks))
                } else {
                    Cmd::quit()
                }
            }
            Fx::Done | Fx::Failed(_) => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log.len(), 5);
    }

    #[test]
    fn repaint_clears_the_screen_and_draws_everything_again() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx.clone());
        let (frames_tx, frames) = channel();
        let mut app = Scripted::new(|m| match m {
            Fx::Start => Cmd::repaint(),
            Fx::Done | Fx::Failed(_) => Cmd::quit(),
            Fx::Got(_) => Cmd::none(),
        });
        app.frames = Some(frames_tx);
        tx.send(Input::Message(Fx::Start)).unwrap();
        let handle = spawn_driver(tx.clone(), move |tx| {
            // The initial frame, then the one the repaint asked for.
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            frames.recv_timeout(Duration::from_secs(10)).unwrap();
            tx.send(Input::Message(Fx::Done)).unwrap();
        });
        event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        handle.join().unwrap();
        let clears = host.out.windows(4).filter(|w| *w == b"\x1b[2J").count();
        assert_eq!(clears, 2, "{:?}", String::from_utf8_lossy(&host.out));
    }

    #[test]
    fn copy_to_clipboard_writes_the_osc_52_sequences_in_order() {
        let mut host = FakeHost::new(20, 3);
        let (tx, rx) = channel();
        let effects = Effects::new(tx.clone());
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([
                Cmd::copy_to_clipboard("first"),
                Cmd::copy_to_clipboard(String::new()),
                Cmd::copy_to_clipboard("x\x07\x1b[2J"),
                Cmd::quit(),
            ]),
            _ => Cmd::none(),
        });
        tx.send(Input::Message(Fx::Start)).unwrap();
        event_loop(app, &rx, &effects, &mut host, 0).unwrap();
        let out = String::from_utf8_lossy(&host.out).into_owned();
        let first = out.find("\x1b]52;c;Zmlyc3Q=\x1b\\").expect(&out);
        let second = out.find("\x1b]52;c;eAcbWzJK\x1b\\").expect(&out);
        assert!(first < second);
        assert_eq!(out.matches("]52;").count(), 2, "{out:?}");
    }

    #[test]
    fn a_worker_can_stream_many_messages_and_they_arrive_in_order() {
        const N: u32 = 10_000;
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::spawn(|tx| {
                for i in 0..N {
                    tx.send(Fx::Got(i)).unwrap();
                }
                tx.send(Fx::Done).unwrap();
            }),
            Fx::Done | Fx::Failed(_) => Cmd::quit(),
            Fx::Got(_) => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log.len(), N as usize + 2);
        assert_eq!(app.log[0], Fx::Start);
        assert!(
            app.log[1..=N as usize]
                .iter()
                .enumerate()
                .all(|(i, m)| *m == Fx::Got(i as u32))
        );
        assert_eq!(app.log.last(), Some(&Fx::Done));
    }

    #[test]
    fn a_streaming_worker_learns_the_program_ended_when_send_fails() {
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = stopped.clone();
        let app = Scripted::new(move |m| match m {
            Fx::Start => {
                let flag = flag.clone();
                Cmd::spawn(move |tx| {
                    let mut i = 0;
                    while tx.send(Fx::Got(i)).is_ok() {
                        i = i.wrapping_add(1);
                        thread::yield_now();
                    }
                    flag.store(true, Ordering::SeqCst);
                })
            }
            Fx::Got(3) => Cmd::quit(),
            _ => Cmd::none(),
        });
        let (tx, rx) = channel();
        tx.send(Input::Message(Fx::Start)).unwrap();
        let effects = Effects::new(tx);
        event_loop(app, &rx, &effects, &mut FakeHost::new(20, 3), 0).unwrap();
        drop(effects);
        drop(rx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !stopped.load(Ordering::SeqCst) {
            assert!(Instant::now() < deadline, "the worker never saw Closed");
            thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn timers_still_pending_when_the_program_ends_never_fire() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([Cmd::after(Duration::from_millis(60), Fx::Done), Cmd::quit()]),
            _ => Cmd::none(),
        });
        let (tx, rx) = channel();
        tx.send(Input::Message(Fx::Start)).unwrap();
        let effects = Effects::new(tx);
        event_loop(app, &rx, &effects, &mut FakeHost::new(20, 3), 0).unwrap();
        drop(effects);
        thread::sleep(Duration::from_millis(150));
        // Had the timer fired, its message would be waiting here.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_delay_too_large_to_represent_is_a_timer_that_never_fires() {
        let app = Scripted::new(|m| match m {
            Fx::Start => Cmd::batch([
                Cmd::after(Duration::MAX, Fx::Done),
                Cmd::after(Duration::from_millis(20), Fx::Got(1)),
            ]),
            Fx::Got(_) => Cmd::quit(),
            Fx::Done | Fx::Failed(_) => Cmd::none(),
        });
        let app = drive(app, &mut FakeHost::new(20, 3));
        assert_eq!(app.log, vec![Fx::Start, Fx::Got(1)]);
    }

    #[test]
    fn cmd_debug_names_every_variant() {
        assert_eq!(format!("{:?}", Cmd::<Msg>::repaint()), "Cmd::repaint()");
        assert_eq!(
            format!("{:?}", Cmd::<Msg>::batch([Cmd::none(), Cmd::quit()])),
            "Cmd::batch(2 commands)"
        );
        assert_eq!(
            format!("{:?}", Cmd::perform(|| Msg::Up)),
            "Cmd::perform(..)"
        );
        assert_eq!(
            format!("{:?}", Cmd::spawn(|_: Sender<Msg>| {})),
            "Cmd::spawn(..)"
        );
        assert_eq!(
            format!("{:?}", Cmd::after(Duration::from_millis(5), Msg::Up)),
            "Cmd::after(5ms, ..)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_program_sender_reaches_update_from_other_threads_before_and_during_the_run() {
        let pty = Pty::open();
        pty.set_size(40, 4);
        let master = pty.master;
        let drain = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut seen = Vec::new();
            while !seen.ends_with(b"\x1b[?1049l") {
                seen.extend(drain_fd(master, 50));
                assert!(Instant::now() < deadline, "the program never quit");
            }
        });
        let program = Program::new(Counter::new()).max_fps(0);
        let sender = program.sender();
        // Queued before the loop starts.
        sender.send(Msg::Up).unwrap();
        let workers: Vec<_> = (0..2)
            .map(|_| {
                let sender = sender.clone();
                thread::spawn(move || {
                    for _ in 0..500 {
                        sender.send(Msg::Up).unwrap();
                    }
                })
            })
            .collect();
        let quitter = thread::spawn(move || {
            for w in workers {
                w.join().unwrap();
            }
            sender.send(Msg::Quit).unwrap();
        });
        let app = program.run_on(pty.slave, pty.slave, false).unwrap();
        quitter.join().unwrap();
        drain.join().unwrap();
        assert_eq!(app.n, 1001);
    }

    #[cfg(unix)]
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

//! Reads the terminal on its own thread and turns what it sees into
//! [`Input`] values.
//!
//! One thread waits on stdin, the signal pipe and a wake-up pipe at once, so
//! the event loop has a single place to block and nothing polls on a timer.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};

use crate::runtime::Input;
use crate::signals::new_pipe;
use crate::{Parser, Signals};

/// How long a lone Esc waits to see whether a sequence follows.
const ESCAPE_TIMEOUT_MS: libc::c_int = 50;
/// How long a paste may sit idle before it is given up on.
const PASTE_IDLE_MS: libc::c_int = 1000;
const READ_SIZE: usize = 4096;

/// The reader thread. Dropping it stops the thread and waits for it.
pub(crate) struct InputReader {
    wake_write: RawFd,
    wake_read: RawFd,
    thread: Option<JoinHandle<()>>,
}

impl InputReader {
    /// Starts reading `input`, and `signals` if given, sending to `tx`.
    pub(crate) fn spawn<M: Send + 'static>(
        input: RawFd,
        signals: Option<Signals>,
        tx: Sender<Input<M>>,
    ) -> io::Result<InputReader> {
        let (wake_read, wake_write) = new_pipe()?;
        let thread = thread::Builder::new()
            .name("crewtui-input".into())
            .spawn(move || read_loop(input, wake_read, signals, &tx))
            .inspect_err(|_| close_pair(wake_read, wake_write))?;
        Ok(InputReader {
            wake_write,
            wake_read,
            thread: Some(thread),
        })
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        // SAFETY: writes one byte from a live local to a pipe we own. If the
        // pipe is full a wake-up is already pending, so a failure is fine.
        unsafe {
            let byte = 1u8;
            libc::write(self.wake_write, (&byte as *const u8).cast(), 1);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        close_pair(self.wake_read, self.wake_write);
    }
}

fn close_pair(a: RawFd, b: RawFd) {
    // SAFETY: both descriptors belong to the caller and are closed once.
    unsafe {
        libc::close(a);
        libc::close(b);
    }
}

fn poll(fds: &mut [libc::pollfd], timeout: libc::c_int) -> io::Result<usize> {
    loop {
        // SAFETY: the pointer and length come from a live slice.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn read_some(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        // SAFETY: the pointer and length come from a live buffer.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn read_loop<M>(input: RawFd, wake: RawFd, mut signals: Option<Signals>, tx: &Sender<Input<M>>) {
    let mut parser = Parser::new();
    let mut buf = [0u8; READ_SIZE];
    let readable =
        |p: &libc::pollfd| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
    loop {
        let timeout = if parser.is_pasting() {
            PASTE_IDLE_MS
        } else if parser.is_waiting() {
            ESCAPE_TIMEOUT_MS
        } else {
            -1
        };
        let mut fds = vec![
            libc::pollfd {
                fd: input,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if let Some(s) = &signals {
            fds.push(libc::pollfd {
                fd: s.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }
        let ready = match poll(&mut fds, timeout) {
            Ok(n) => n,
            Err(e) => {
                let _ = tx.send(Input::Failed(e));
                return;
            }
        };
        if readable(&fds[1]) {
            return;
        }
        if ready == 0 {
            let event = if parser.is_pasting() {
                parser.abort_paste()
            } else {
                parser.escape_timeout()
            };
            for event in event
                .into_iter()
                .chain(std::iter::from_fn(|| parser.next_event()))
            {
                if tx.send(Input::Event(event)).is_err() {
                    return;
                }
            }
            continue;
        }
        if let (Some(s), Some(p)) = (signals.as_mut(), fds.get(2)) {
            if readable(p) {
                match s.pending() {
                    Ok(list) => {
                        for signal in list {
                            if tx.send(Input::Signal(signal)).is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Input::Failed(e));
                        return;
                    }
                }
            }
        }
        if readable(&fds[0]) {
            match read_some(input, &mut buf) {
                Ok(0) => {
                    let _ = tx.send(Input::Failed(io::ErrorKind::UnexpectedEof.into()));
                    return;
                }
                Ok(n) => {
                    parser.feed(&buf[..n]);
                    while let Some(event) = parser.next_event() {
                        if tx.send(Input::Event(event)).is_err() {
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(Input::Failed(e));
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::serial;
    use crate::{Event, KeyCode, KeyEvent, Modifiers};
    use std::sync::mpsc::{Receiver, channel};
    use std::time::{Duration, Instant};

    /// A pipe standing in for the terminal's input.
    struct Feed {
        read: RawFd,
        write: RawFd,
    }

    impl Feed {
        fn new() -> Feed {
            let (read, write) = new_pipe().unwrap();
            Feed { read, write }
        }

        fn send(&self, bytes: &[u8]) {
            // SAFETY: the pointer and length come from a live slice.
            let n = unsafe { libc::write(self.write, bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n, bytes.len() as isize);
        }

        fn hang_up(&mut self) {
            // SAFETY: closes the write end once; `Drop` skips it afterwards.
            unsafe { libc::close(self.write) };
            self.write = -1;
        }
    }

    impl Drop for Feed {
        fn drop(&mut self) {
            // SAFETY: closes descriptors this value owns, once each.
            unsafe {
                libc::close(self.read);
                if self.write >= 0 {
                    libc::close(self.write);
                }
            }
        }
    }

    fn next(rx: &Receiver<Input<()>>) -> Input<()> {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("no input arrived")
    }

    fn next_event(rx: &Receiver<Input<()>>) -> Event {
        match next(rx) {
            Input::Event(e) => e,
            Input::Signal(s) => panic!("unexpected signal {s:?}"),
            Input::Failed(e) => panic!("unexpected failure {e}"),
            Input::Message(()) => panic!("unexpected message"),
        }
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: Modifiers::NONE,
        })
    }

    #[test]
    fn bytes_arrive_as_events() {
        let feed = Feed::new();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, None, tx).unwrap();
        feed.send(b"a\x1b[A\r");
        assert_eq!(next_event(&rx), key(KeyCode::Char('a')));
        assert_eq!(next_event(&rx), key(KeyCode::Up));
        assert_eq!(next_event(&rx), key(KeyCode::Enter));
    }

    #[test]
    fn a_lone_escape_becomes_esc_after_a_short_wait() {
        let feed = Feed::new();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, None, tx).unwrap();
        let sent = Instant::now();
        feed.send(b"\x1b");
        assert_eq!(next_event(&rx), key(KeyCode::Esc));
        let waited = sent.elapsed();
        assert!(waited >= Duration::from_millis(30), "{waited:?}");
        assert!(waited < Duration::from_secs(2), "{waited:?}");
    }

    #[test]
    fn a_sequence_split_across_reads_is_not_mistaken_for_esc() {
        let feed = Feed::new();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, None, tx).unwrap();
        feed.send(b"\x1b");
        std::thread::sleep(Duration::from_millis(10));
        feed.send(b"[A");
        assert_eq!(next_event(&rx), key(KeyCode::Up));
    }

    #[test]
    fn a_paste_is_one_event_and_a_lost_end_marker_is_given_up_on() {
        let feed = Feed::new();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, None, tx).unwrap();
        feed.send(b"\x1b[200~one\x1b[201~");
        assert_eq!(next_event(&rx), Event::Paste("one".into()));
        feed.send(b"\x1b[200~partial");
        assert_eq!(next_event(&rx), Event::Paste("partial".into()));
        feed.send(b"x");
        assert_eq!(next_event(&rx), key(KeyCode::Char('x')));
    }

    #[test]
    fn end_of_input_is_reported_as_a_failure() {
        let mut feed = Feed::new();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, None, tx).unwrap();
        feed.hang_up();
        match next(&rx) {
            Input::Failed(e) => assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof),
            _ => panic!("expected a failure"),
        }
    }

    #[test]
    fn signals_arrive_through_the_same_channel() {
        let _lock = serial();
        let feed = Feed::new();
        let signals = Signals::install().unwrap();
        let (tx, rx) = channel();
        let _reader = InputReader::spawn(feed.read, Some(signals), tx).unwrap();
        // SAFETY: `raise` takes a signal number and has no other preconditions.
        assert_eq!(unsafe { libc::raise(libc::SIGWINCH) }, 0);
        assert!(matches!(next(&rx), Input::Signal(crate::Signal::Resize)));
        feed.send(b"k");
        assert_eq!(next_event(&rx), key(KeyCode::Char('k')));
    }

    #[test]
    fn dropping_the_reader_stops_it_promptly_and_gives_the_signals_back() {
        let _lock = serial();
        let feed = Feed::new();
        let signals = Signals::install().unwrap();
        let (tx, _rx) = channel::<Input<()>>();
        let reader = InputReader::spawn(feed.read, Some(signals), tx).unwrap();
        let started = Instant::now();
        drop(reader);
        assert!(started.elapsed() < Duration::from_secs(1));
        // The thread owned the handlers; they can be installed again.
        drop(Signals::install().unwrap());
    }

    #[test]
    fn a_reader_whose_receiver_is_gone_still_shuts_down() {
        let feed = Feed::new();
        let (tx, rx) = channel::<Input<()>>();
        let reader = InputReader::spawn(feed.read, None, tx).unwrap();
        drop(rx);
        feed.send(b"a");
        std::thread::sleep(Duration::from_millis(50));
        drop(reader);
    }
}

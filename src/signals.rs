//! Delivers process signals as values through a self-pipe.
//!
//! A signal handler may only call async-signal-safe functions, which rules
//! out nearly everything an app wants to do in response. The handler here
//! writes one byte to a pipe and returns; the event loop polls the read end
//! next to its other inputs and turns the bytes back into [`Signal`]s.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};

use crate::signal::Signal;

/// The signals this module installs handlers for, in the order of the byte
/// the handler writes for each.
const ALL: [(Signal, libc::c_int); 6] = [
    (Signal::Interrupt, libc::SIGINT),
    (Signal::Terminate, libc::SIGTERM),
    (Signal::Hangup, libc::SIGHUP),
    (Signal::Resize, libc::SIGWINCH),
    (Signal::Continue, libc::SIGCONT),
    (Signal::Suspend, libc::SIGTSTP),
];

fn from_byte(b: u8) -> Option<Signal> {
    ALL.get(b as usize).map(|(s, _)| *s)
}

/// The signal that stops the process. It is SIGTSTP, so a job-control shell
/// sees an ordinary stop. The default action of SIGTSTP is discarded in an
/// orphaned process group, which is what the child of a test on a pty is, so
/// tests switch to SIGSTOP.
#[cfg(not(test))]
fn stop_signal() -> libc::c_int {
    libc::SIGTSTP
}

#[cfg(test)]
static STOP_WITH: std::sync::atomic::AtomicI32 = AtomicI32::new(libc::SIGTSTP);

#[cfg(test)]
fn stop_signal() -> libc::c_int {
    STOP_WITH.load(Ordering::SeqCst)
}

/// Makes a test child stop with SIGSTOP instead of SIGTSTP.
#[cfg(test)]
pub(crate) fn stop_with_sigstop() {
    STOP_WITH.store(libc::SIGSTOP, Ordering::SeqCst);
}

/// How many times the SIGCONT handler ran, so that `stop_process` can tell
/// whether a continue signal was delivered while it was stopped.
static CONTINUES: AtomicU32 = AtomicU32::new(0);

/// Write end of the pipe, read by the handler. `-1` while nothing is installed.
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
static INSTALLED: AtomicBool = AtomicBool::new(false);
/// The pipe lives as long as the process. A handler that is already running
/// when a `Signals` is dropped may still write to it, and closing the
/// descriptor would let that byte land in whatever file gets the number
/// next. Leaking two descriptors is the price of never having that race.
static PIPE: OnceLock<(RawFd, RawFd)> = OnceLock::new();

#[cfg(any(target_os = "linux", target_os = "dragonfly", target_os = "emscripten"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns the address of the calling thread's errno.
    unsafe { libc::__errno_location() }
}

#[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns the address of the calling thread's errno.
    unsafe { libc::__errno() }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
fn errno_location() -> *mut libc::c_int {
    // SAFETY: returns the address of the calling thread's errno.
    unsafe { libc::__error() }
}

/// Only async-signal-safe calls: an atomic load, `write`, and errno
/// save and restore.
extern "C" fn handler(sig: libc::c_int) {
    let Some(code) = ALL.iter().position(|&(_, s)| s == sig) else {
        return;
    };
    if sig == libc::SIGCONT {
        CONTINUES.fetch_add(1, Ordering::SeqCst);
    }
    let fd = WRITE_FD.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    // SAFETY: `errno_location` gives a valid pointer for this thread, and
    // `write` reads one byte from a live local. A full pipe makes `write`
    // fail with EAGAIN, which is fine: the pending bytes already say that
    // something happened.
    unsafe {
        let errno = errno_location();
        let saved = *errno;
        let byte = code as u8;
        libc::write(fd, (&byte as *const u8).cast(), 1);
        *errno = saved;
    }
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
pub(crate) fn new_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe2` writes. Both
    // flags are set atomically, so no other thread can fork in between and
    // hand the pipe to a child.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((fds[0], fds[1]))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
pub(crate) fn new_pipe() -> io::Result<(RawFd, RawFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `fds` has room for the two descriptors `pipe` writes; `fcntl`
    // works on descriptors we just created. There is no `pipe2` here, so a
    // fork on another thread between these calls could leak the pipe.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        for fd in fds {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            let fdflags = libc::fcntl(fd, libc::F_GETFD);
            if flags < 0
                || fdflags < 0
                || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0
                || libc::fcntl(fd, libc::F_SETFD, fdflags | libc::FD_CLOEXEC) < 0
            {
                let err = io::Error::last_os_error();
                libc::close(fds[0]);
                libc::close(fds[1]);
                return Err(err);
            }
        }
    }
    Ok((fds[0], fds[1]))
}

/// The process-wide pipe, created on first use.
fn pipe() -> io::Result<(RawFd, RawFd)> {
    if let Some(p) = PIPE.get() {
        return Ok(*p);
    }
    let created = new_pipe()?;
    // Two threads can race here; the loser closes its extra pipe.
    match PIPE.set(created) {
        Ok(()) => Ok(created),
        Err(_) => {
            // SAFETY: the descriptors were just created and never shared.
            unsafe {
                libc::close(created.0);
                libc::close(created.1);
            }
            Ok(*PIPE.get().expect("set by the other thread"))
        }
    }
}

/// Handlers for [`Signal`]s, and the pipe they report through.
///
/// Only one can exist per process. Dropping it puts the previous handlers
/// back. Poll [`AsRawFd::as_raw_fd`] for readability, then call
/// [`Signals::pending`].
///
/// A SIGINT, SIGHUP or SIGTSTP that was being ignored when
/// [`Signals::install`] ran, as SIGHUP is under `nohup` and SIGINT is for a
/// background job in a shell script, stays ignored, so [`Signal::Interrupt`], [`Signal::Hangup`] and
/// [`Signal::Suspend`] are never delivered then.
#[derive(Debug)]
pub struct Signals {
    read_fd: RawFd,
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

impl Signals {
    /// Installs the handlers.
    pub fn install() -> io::Result<Signals> {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "signal handlers are already installed",
            ));
        }
        Signals::install_inner().inspect_err(|_| INSTALLED.store(false, Ordering::SeqCst))
    }

    fn install_inner() -> io::Result<Signals> {
        let (read_fd, write_fd) = pipe()?;
        let mut signals = Signals {
            read_fd,
            previous: Vec::new(),
        };
        // Bytes left by an earlier `Signals` are stale.
        signals.pending()?;
        WRITE_FD.store(write_fd, Ordering::SeqCst);

        for (_, sig) in ALL {
            // SAFETY: `action` is fully initialized before use, `old` is
            // valid for a write, and `handler` has the signature `sigaction`
            // expects for a handler without `SA_SIGINFO`.
            let old = unsafe {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let ignored = old.sa_sigaction == libc::SIG_IGN;
                if ignored && (sig == libc::SIGINT || sig == libc::SIGHUP || sig == libc::SIGTSTP) {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = handler as extern "C" fn(libc::c_int) as libc::sighandler_t;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(sig, &action, std::ptr::null_mut()) != 0 {
                    return Err(io::Error::last_os_error());
                }
                old
            };
            signals.previous.push((sig, old));
        }
        Ok(signals)
    }

    /// Stops the whole process the way an unhandled SIGTSTP would, and returns
    /// once it is continued. Our handler for SIGTSTP is put aside while it is
    /// stopped, since with it installed the process would not stop, and put
    /// back after.
    ///
    /// Returns whether the SIGCONT handler ran meanwhile, which means a
    /// [`Signal::Continue`] is on its way through the pipe. In an orphaned
    /// process group the stop is discarded and nothing follows.
    pub(crate) fn stop_process() -> io::Result<bool> {
        let continues = CONTINUES.load(Ordering::SeqCst);
        // SAFETY: both actions are fully initialized before use, and `ours`
        // is valid for a write.
        unsafe {
            let mut default: libc::sigaction = std::mem::zeroed();
            default.sa_sigaction = libc::SIG_DFL;
            libc::sigemptyset(&mut default.sa_mask);
            let mut ours: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGTSTP, &default, &mut ours) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::raise(stop_signal());
            // Execution goes on here after SIGCONT.
            if libc::sigaction(libc::SIGTSTP, &ours, std::ptr::null_mut()) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        // The handler may run on another thread, a moment after `raise`
        // returned. In an orphaned process group nothing runs, and this
        // waits for nothing.
        for _ in 0..250 {
            if CONTINUES.load(Ordering::SeqCst) != continues {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        Ok(false)
    }

    /// Signals that arrived since the last call, oldest first. Never blocks.
    pub fn pending(&mut self) -> io::Result<Vec<Signal>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: the pointer and length come from a live buffer.
            let n = unsafe { libc::read(self.read_fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                out.extend(buf[..n as usize].iter().filter_map(|&b| from_byte(b)));
                continue;
            }
            if n == 0 {
                return Ok(out);
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::WouldBlock => return Ok(out),
                io::ErrorKind::Interrupted => continue,
                _ => return Err(err),
            }
        }
    }
}

impl AsRawFd for Signals {
    fn as_raw_fd(&self) -> RawFd {
        self.read_fd
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        for (sig, old) in &self.previous {
            // SAFETY: `old` is the action `sigaction` returned for `sig`.
            unsafe { libc::sigaction(*sig, old, std::ptr::null_mut()) };
        }
        // A handler that already started may still write, so the pipe stays
        // open; see `PIPE`.
        WRITE_FD.store(-1, Ordering::SeqCst);
        INSTALLED.store(false, Ordering::SeqCst);
    }
}

/// Handlers are per process, so tests anywhere in the crate that install
/// them take this lock and run one at a time.
#[cfg(test)]
pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
    use std::sync::{Mutex, PoisonError};
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raise(sig: libc::c_int) {
        // SAFETY: `raise` takes a signal number and has no other preconditions.
        assert_eq!(unsafe { libc::raise(sig) }, 0);
    }

    fn disposition(sig: libc::c_int) -> libc::sighandler_t {
        // SAFETY: querying the current action with a null new action is valid.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(sig, std::ptr::null(), &mut old), 0);
            old.sa_sigaction
        }
    }

    #[test]
    fn each_signal_arrives_as_its_own_value() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        for (expected, sig) in ALL {
            raise(sig);
            assert_eq!(signals.pending().unwrap(), vec![expected], "{expected:?}");
        }
    }

    #[test]
    fn signals_keep_their_arrival_order() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        raise(libc::SIGWINCH);
        raise(libc::SIGTERM);
        raise(libc::SIGWINCH);
        assert_eq!(
            signals.pending().unwrap(),
            vec![Signal::Resize, Signal::Terminate, Signal::Resize]
        );
        assert!(signals.pending().unwrap().is_empty());
    }

    #[test]
    fn pending_does_not_block_when_nothing_arrived() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        assert!(signals.pending().unwrap().is_empty());
    }

    #[test]
    fn the_read_end_can_be_polled() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        let mut pfd = libc::pollfd {
            fd: signals.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pfd` is valid for one entry.
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 0) }, 0);
        raise(libc::SIGHUP);
        // SAFETY: as above.
        assert_eq!(unsafe { libc::poll(&mut pfd, 1, 0) }, 1);
        assert_eq!(signals.pending().unwrap(), vec![Signal::Hangup]);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_handler_leaves_errno_alone_even_when_its_write_fails() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        // A successful write doesn't touch errno, so fill the pipe first:
        // from then on the handler's write fails with EAGAIN.
        for _ in 0..100_000 {
            raise(libc::SIGWINCH);
        }
        // SAFETY: errno is thread-local and always writable.
        unsafe { *libc::__errno_location() = 4242 };
        raise(libc::SIGWINCH);
        // SAFETY: as above.
        assert_eq!(unsafe { *libc::__errno_location() }, 4242);
        signals.pending().unwrap();
    }

    #[test]
    fn a_full_pipe_does_not_block_the_handler() {
        let _s = serial();
        let mut signals = Signals::install().unwrap();
        // A pipe holds 64 KiB on Linux; this is well past that.
        for _ in 0..100_000 {
            raise(libc::SIGWINCH);
        }
        let got = signals.pending().unwrap();
        assert!(!got.is_empty() && got.iter().all(|&s| s == Signal::Resize));
    }

    #[test]
    fn a_second_install_fails_until_the_first_is_dropped_and_handlers_come_back() {
        let _s = serial();
        let before: Vec<_> = ALL.iter().map(|&(_, s)| disposition(s)).collect();
        let first = Signals::install().unwrap();
        let err = Signals::install().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_ne!(disposition(libc::SIGTERM), before[1]);
        drop(first);
        let after: Vec<_> = ALL.iter().map(|&(_, s)| disposition(s)).collect();
        assert_eq!(before, after);
        drop(Signals::install().unwrap());
    }

    #[test]
    fn an_ignored_sigint_sighup_or_sigtstp_stays_ignored() {
        let _s = serial();
        // SAFETY: setting a disposition to ignore has no other preconditions.
        let (old_int, old_hup, old_tstp) = unsafe {
            (
                libc::signal(libc::SIGINT, libc::SIG_IGN),
                libc::signal(libc::SIGHUP, libc::SIG_IGN),
                libc::signal(libc::SIGTSTP, libc::SIG_IGN),
            )
        };
        let mut signals = Signals::install().unwrap();
        assert_eq!(disposition(libc::SIGINT), libc::SIG_IGN);
        assert_eq!(disposition(libc::SIGHUP), libc::SIG_IGN);
        assert_eq!(disposition(libc::SIGTSTP), libc::SIG_IGN);
        raise(libc::SIGINT);
        raise(libc::SIGHUP);
        raise(libc::SIGTSTP);
        raise(libc::SIGTERM);
        assert_eq!(signals.pending().unwrap(), vec![Signal::Terminate]);
        drop(signals);
        assert_eq!(disposition(libc::SIGINT), libc::SIG_IGN);
        // SAFETY: putting back what the test replaced.
        unsafe {
            libc::signal(libc::SIGINT, old_int);
            libc::signal(libc::SIGHUP, old_hup);
            libc::signal(libc::SIGTSTP, old_tstp);
        }
    }

    /// The child of `stop_process_stops_with_sigtstp_and_puts_the_handler_back`.
    /// It stops itself and then reports whether our handler is back.
    #[test]
    fn stop_probe_child() {
        if std::env::var("CREWTUI_STOP_PROBE").is_err() {
            return;
        }
        let _signals = Signals::install().unwrap();
        let continued = Signals::stop_process().unwrap();
        // Our handler is installed again: not the default and not ignored.
        let d = disposition(libc::SIGTSTP);
        let handler_back = d != libc::SIG_DFL && d != libc::SIG_IGN;
        std::process::exit(match (handler_back, continued) {
            (true, true) => 0,
            (false, _) => 3,
            (_, false) => 4,
        });
    }

    /// The real thing: a process that isn't orphaned really stops with
    /// SIGTSTP, and after SIGCONT it finds its handler back and knows it was
    /// continued.
    #[test]
    fn stop_process_stops_with_sigtstp_and_puts_the_handler_back() {
        use crate::term_model::{kill, wait_timeout, wait_until_stopped};
        use std::os::unix::process::CommandExt;
        use std::time::Duration;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "signals::tests::stop_probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("CREWTUI_STOP_PROBE", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Its own group, whose parent is in another group of this session:
            // not orphaned, so the default action of SIGTSTP stops it.
            .process_group(0)
            .spawn()
            .unwrap();
        let stopped = wait_until_stopped(&child, Duration::from_secs(30));
        assert_eq!(stopped, Some(libc::SIGTSTP));
        kill(&child, libc::SIGCONT);
        let status = wait_timeout(&mut child, Duration::from_secs(30));
        assert_eq!(status.and_then(|s| s.code()), Some(0));
    }

    #[test]
    fn bytes_left_over_from_an_earlier_install_are_not_delivered() {
        let _s = serial();
        let first = Signals::install().unwrap();
        raise(libc::SIGTERM);
        drop(first);
        let mut second = Signals::install().unwrap();
        assert!(second.pending().unwrap().is_empty());
    }

    #[test]
    fn signals_are_errors_that_say_what_happened() {
        assert_eq!(Signal::Terminate.to_string(), "terminated (SIGTERM)");
        let err: Box<dyn std::error::Error> = Box::new(Signal::Interrupt);
        assert!(err.to_string().contains("SIGINT"));
    }
}

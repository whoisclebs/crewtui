//! Delivers console control events as [`Signal`] values.
//!
//! The system runs the handler on a thread of its own, so it only records
//! what happened and sets an event. The input reader waits on that event
//! next to the console's input and turns it back into signals.
//!
//! The event is created once for the process and never closed. The handler
//! can be running on the system's thread while [`Signals`] is dropped, and
//! removing a handler does not wait for it, so an event that could be closed
//! and its value reused would be signalled by mistake.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, Sleep};

use crate::signal::Signal;
use crate::terminal::Handle;

const INTERRUPT: u32 = 1;
const TERMINATE: u32 = 1 << 1;
const HANGUP: u32 = 1 << 2;

static INSTALLED: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicU32 = AtomicU32::new(0);
static EVENT: AtomicPtr<core::ffi::c_void> = AtomicPtr::new(std::ptr::null_mut());

unsafe extern "system" fn handler(ctrl_type: u32) -> i32 {
    let (bit, process_ends) = match ctrl_type {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => (INTERRUPT, false),
        CTRL_CLOSE_EVENT => (HANGUP, true),
        CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => (TERMINATE, true),
        _ => return 0,
    };
    PENDING.fetch_or(bit, Ordering::SeqCst);
    let event = EVENT.load(Ordering::SeqCst);
    if !event.is_null() {
        // SAFETY: `event` was created by `event` and is never closed.
        unsafe { SetEvent(event) };
    }
    if process_ends {
        // The system ends the process when this returns. Give the loop the
        // time it needs to restore the console first. If the program exits
        // sooner, the process ends sooner.
        // SAFETY: only waits.
        unsafe { Sleep(4000) };
    }
    1
}

/// The event the handler sets, created on first use.
pub(crate) fn event() -> io::Result<Handle> {
    let current = EVENT.load(Ordering::SeqCst);
    if !current.is_null() {
        return Ok(Handle(current));
    }
    // SAFETY: an automatic-reset event with no name and no security
    // attributes.
    let created = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
    if created.is_null() {
        return Err(io::Error::last_os_error());
    }
    match EVENT.compare_exchange(
        std::ptr::null_mut(),
        created,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => Ok(Handle(created)),
        // Another thread was first. Its event is the one in use, and this
        // one is closed again.
        Err(existing) => {
            // SAFETY: closes the event created above, which nobody has seen.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(created) };
            Ok(Handle(existing))
        }
    }
}

/// Signals that arrived since the last call. Never blocks.
pub(crate) fn take_pending() -> Vec<Signal> {
    let bits = PENDING.swap(0, Ordering::SeqCst);
    let mut out = Vec::new();
    for (bit, signal) in [
        (INTERRUPT, Signal::Interrupt),
        (TERMINATE, Signal::Terminate),
        (HANGUP, Signal::Hangup),
    ] {
        if bits & bit != 0 {
            out.push(signal);
        }
    }
    out
}

/// The handler for console control events. Only one can exist per process.
/// Dropping it uninstalls the handler.
#[derive(Debug)]
pub(crate) struct Signals {
    _private: (),
}

impl Signals {
    /// Installs the handler.
    pub(crate) fn install() -> io::Result<Signals> {
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "signal handlers are already installed",
            ));
        }
        if let Err(e) = event() {
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(e);
        }
        PENDING.store(0, Ordering::SeqCst);
        // SAFETY: `handler` has the signature the system calls it with.
        if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
            let err = io::Error::last_os_error();
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(err);
        }
        Ok(Signals { _private: () })
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        // SAFETY: removes the handler installed above.
        unsafe { SetConsoleCtrlHandler(Some(handler), 0) };
        INSTALLED.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    // The handler and its state are per process, so tests take turns.
    static TURN: Mutex<()> = Mutex::new(());

    fn is_set() -> bool {
        // SAFETY: the event is never closed.
        unsafe { WaitForSingleObject(event().unwrap().0, 0) == 0 }
    }

    #[test]
    fn a_control_event_becomes_a_signal_and_sets_the_event() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let _signals = Signals::install().unwrap();
        assert!(!is_set());
        // SAFETY: what the system does when the user presses Ctrl+C.
        assert_eq!(unsafe { handler(CTRL_C_EVENT) }, 1);
        assert!(is_set());
        assert_eq!(take_pending(), [Signal::Interrupt]);
        assert_eq!(take_pending(), []);
        // SAFETY: as above, for Ctrl+Break.
        unsafe { handler(CTRL_BREAK_EVENT) };
        assert_eq!(take_pending(), [Signal::Interrupt]);
        // An event that is not one of ours is left to the next handler.
        // SAFETY: as above.
        assert_eq!(unsafe { handler(99) }, 0);
        assert_eq!(take_pending(), []);
    }

    #[test]
    fn only_one_handler_exists_at_a_time_and_a_dropped_one_can_be_replaced() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let first = Signals::install().unwrap();
        let err = Signals::install().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        drop(first);
        let _again = Signals::install().unwrap();
        // What arrived for the old handler is not delivered to the new one.
        assert_eq!(take_pending(), []);
    }

    #[test]
    fn the_event_is_the_same_for_the_whole_process() {
        let a = event().unwrap();
        let b = event().unwrap();
        assert_eq!(a.0, b.0);
    }
}

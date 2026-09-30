//! Delivers console control events as [`Signal`] values.
//!
//! The system runs the handler on a thread of its own, so it only records
//! what happened and sets an event. The input reader waits on that event
//! next to the console's input and turns it back into signals.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
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
    let event: HANDLE = EVENT.load(Ordering::SeqCst);
    if !event.is_null() {
        // SAFETY: `event` is the handle `install` created, and it is only
        // closed after this handler is removed.
        unsafe { SetEvent(event) };
    }
    if process_ends {
        // The system ends the process when this returns. Give the loop the
        // time it needs to restore the console first.
        // SAFETY: only waits.
        unsafe { Sleep(4000) };
    }
    1
}

/// The handler for console control events, and the event it signals. Only
/// one can exist per process.
#[derive(Debug)]
pub(crate) struct Signals {
    event: Handle,
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
        // SAFETY: an automatic-reset event with no name and no security
        // attributes.
        let event = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if event.is_null() {
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(io::Error::last_os_error());
        }
        PENDING.store(0, Ordering::SeqCst);
        EVENT.store(event, Ordering::SeqCst);
        // SAFETY: `handler` has the signature the system calls it with.
        if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
            let err = io::Error::last_os_error();
            EVENT.store(std::ptr::null_mut(), Ordering::SeqCst);
            // SAFETY: closes the event created above, once.
            unsafe { CloseHandle(event) };
            INSTALLED.store(false, Ordering::SeqCst);
            return Err(err);
        }
        Ok(Signals {
            event: Handle(event),
        })
    }

    /// The event that is set when a signal arrived.
    pub(crate) fn event(&self) -> Handle {
        self.event
    }

    /// Signals that arrived since the last call. Never blocks.
    pub(crate) fn pending(&mut self) -> io::Result<Vec<Signal>> {
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
        Ok(out)
    }
}

impl Drop for Signals {
    fn drop(&mut self) {
        // SAFETY: removes the handler installed above, then closes the event
        // it used, once.
        unsafe {
            SetConsoleCtrlHandler(Some(handler), 0);
            EVENT.store(std::ptr::null_mut(), Ordering::SeqCst);
            CloseHandle(self.event.0);
        }
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

    fn is_set(signals: &Signals) -> bool {
        // SAFETY: the event is open while `signals` is alive.
        unsafe { WaitForSingleObject(signals.event().0, 0) == 0 }
    }

    #[test]
    fn a_control_event_becomes_a_signal_and_sets_the_event() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let mut signals = Signals::install().unwrap();
        assert!(!is_set(&signals));
        // SAFETY: what the system does when the user presses Ctrl+C.
        assert_eq!(unsafe { handler(CTRL_C_EVENT) }, 1);
        assert!(is_set(&signals));
        assert_eq!(signals.pending().unwrap(), [Signal::Interrupt]);
        assert_eq!(signals.pending().unwrap(), []);
        // SAFETY: as above, for Ctrl+Break.
        unsafe { handler(CTRL_BREAK_EVENT) };
        assert_eq!(signals.pending().unwrap(), [Signal::Interrupt]);
        // An event that is not one of ours is left to the next handler.
        // SAFETY: as above.
        assert_eq!(unsafe { handler(99) }, 0);
        assert_eq!(signals.pending().unwrap(), []);
    }

    #[test]
    fn only_one_handler_exists_at_a_time_and_a_dropped_one_can_be_replaced() {
        let _turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let first = Signals::install().unwrap();
        let err = Signals::install().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        drop(first);
        let mut again = Signals::install().unwrap();
        // What arrived for the old handler is not delivered to the new one.
        assert_eq!(again.pending().unwrap(), []);
    }
}

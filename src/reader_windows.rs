//! Reads the console on its own thread and turns what it sees into
//! [`Input`] values.
//!
//! With virtual terminal input the console reports every key as the escape
//! sequence a Unix terminal would send, one character to a key event. Those
//! characters are turned back into bytes and go through the same
//! [`Parser`] as on Unix. Resizes arrive as their own records.
#![allow(unsafe_code)]

use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::Sender;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Console::{
    GetNumberOfConsoleInputEvents, INPUT_RECORD, KEY_EVENT, ReadConsoleInputW,
    WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects,
};

use crate::Parser;
use crate::runtime::Input;
use crate::signal::Signal;
use crate::signals::Signals;
use crate::terminal::Handle;
use crate::utf16::Utf16Decoder;

/// How long a lone Esc waits to see whether a sequence follows.
pub(crate) const ESCAPE_TIMEOUT_MS: u32 = 50;
/// How long a paste may sit idle before it is given up on.
const PASTE_IDLE_MS: u32 = 1000;
const RECORDS: usize = 128;

/// The reader thread. Dropping it stops the thread and waits for it.
pub(crate) struct InputReader {
    wake: Handle,
    thread: Option<JoinHandle<Option<Signals>>>,
}

impl InputReader {
    /// Starts reading `input`, and the events of `signals` if given, sending
    /// to `tx`.
    pub(crate) fn spawn<M: Send + 'static>(
        input: Handle,
        signals: Option<Signals>,
        tx: Sender<Input<M>>,
        keyboard_enhancement: bool,
    ) -> io::Result<InputReader> {
        // SAFETY: an automatic-reset event with no name and no security
        // attributes.
        let wake = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        if wake.is_null() {
            return Err(io::Error::last_os_error());
        }
        let wake = Handle(wake);
        let thread = thread::Builder::new()
            .name("crewtui-input".into())
            .spawn(move || {
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                    read_loop(
                        input,
                        wake,
                        signals,
                        &tx,
                        ESCAPE_TIMEOUT_MS,
                        keyboard_enhancement,
                    )
                }));
                outcome.unwrap_or_else(|_| {
                    // Other senders keep the channel open, so a silent death
                    // would leave the loop waiting forever.
                    let _ = tx.send(Input::Failed(io::Error::other("the input thread panicked")));
                    None
                })
            })
            .inspect_err(|_| {
                // SAFETY: closes the event created above, once.
                unsafe { CloseHandle(wake.0) };
            })?;
        Ok(InputReader {
            wake,
            thread: Some(thread),
        })
    }

    /// Stops the thread and hands back the signal handler it was holding,
    /// so the caller decides when it is uninstalled.
    pub(crate) fn finish(mut self) -> Option<Signals> {
        self.wake();
        self.thread.take().and_then(|t| t.join().ok()).flatten()
    }

    fn wake(&self) {
        // SAFETY: the event is open until `drop`.
        unsafe { SetEvent(self.wake.0) };
    }
}

impl Drop for InputReader {
    fn drop(&mut self) {
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: closes the event created in `spawn`, once, after the
        // thread that waited on it is gone.
        unsafe { CloseHandle(self.wake.0) };
    }
}

/// Runs until told to stop or the input fails, then returns the signal
/// handler rather than dropping it.
fn read_loop<M>(
    input: Handle,
    wake: Handle,
    mut signals: Option<Signals>,
    tx: &Sender<Input<M>>,
    escape_timeout_ms: u32,
    keyboard_enhancement: bool,
) -> Option<Signals> {
    let mut parser = Parser::new().with_keyboard_enhancement(keyboard_enhancement);
    let mut decoder = Utf16Decoder::default();
    // When input last arrived. The Esc and paste timeouts count from here.
    let mut last_input = Instant::now();
    let mut records: Vec<INPUT_RECORD> = vec![unsafe_zeroed_record(); RECORDS];
    loop {
        let limit = if parser.is_pasting() {
            Some(PASTE_IDLE_MS)
        } else if parser.is_waiting() {
            Some(escape_timeout_ms)
        } else {
            None
        };
        let timeout = limit.map_or(INFINITE, |limit| {
            let left = Duration::from_millis(u64::from(limit)).saturating_sub(last_input.elapsed());
            // Round up, so a wake-up just short of the limit doesn't spin.
            left.as_micros()
                .div_ceil(1000)
                .min(u128::from(INFINITE - 1)) as u32
        });
        let mut handles = vec![input.0, wake.0];
        if let Some(s) = &signals {
            handles.push(s.event().0);
        }
        // SAFETY: the pointer and count come from a live vector of handles
        // that stay open while this waits.
        let waited =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout) };
        if waited == WAIT_FAILED {
            let _ = tx.send(Input::Failed(io::Error::last_os_error()));
            return signals;
        }
        if waited == WAIT_TIMEOUT {
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
                    return signals;
                }
            }
            continue;
        }
        match waited - WAIT_OBJECT_0 {
            0 => {
                let mut bytes = Vec::new();
                let mut resized = false;
                match read_records(input, &mut records, &mut decoder, &mut bytes, &mut resized) {
                    Ok(()) => {}
                    Err(e) => {
                        let _ = tx.send(Input::Failed(e));
                        return signals;
                    }
                }
                if resized && tx.send(Input::Signal(Signal::Resize)).is_err() {
                    return signals;
                }
                if !bytes.is_empty() {
                    last_input = Instant::now();
                    parser.feed(&bytes);
                    while let Some(event) = parser.next_event() {
                        if tx.send(Input::Event(event)).is_err() {
                            return signals;
                        }
                    }
                }
            }
            1 => return signals,
            2 => {
                if let Some(s) = signals.as_mut() {
                    match s.pending() {
                        Ok(list) => {
                            for signal in list {
                                if tx.send(Input::Signal(signal)).is_err() {
                                    return signals;
                                }
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Input::Failed(e));
                            return signals;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn unsafe_zeroed_record() -> INPUT_RECORD {
    // SAFETY: an input record is plain integers, for which all zeros is valid.
    unsafe { std::mem::zeroed() }
}

/// Reads what the console has queued, without waiting for more. The
/// characters of key presses go to `bytes` as UTF-8, and `resized` says
/// whether the window changed size.
fn read_records(
    input: Handle,
    records: &mut [INPUT_RECORD],
    decoder: &mut Utf16Decoder,
    bytes: &mut Vec<u8>,
    resized: &mut bool,
) -> io::Result<()> {
    let mut queued = 0u32;
    // SAFETY: `queued` is valid for a write.
    if unsafe { GetNumberOfConsoleInputEvents(input.0, &mut queued) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // The wait can end for an event that is gone by now, and reading with
    // nothing queued would block.
    while queued > 0 {
        let want = (queued as usize).min(records.len()) as u32;
        let mut read = 0u32;
        // SAFETY: the pointer and length come from a live slice, and `read`
        // is valid for a write.
        if unsafe { ReadConsoleInputW(input.0, records.as_mut_ptr(), want, &mut read) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if read == 0 {
            break;
        }
        for record in &records[..read as usize] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    // SAFETY: the event type says which member is valid.
                    let key = unsafe { record.Event.KeyEvent };
                    // SAFETY: both members of the union are integers.
                    let unit = unsafe { key.uChar.UnicodeChar };
                    if key.bKeyDown != 0 && unit != 0 {
                        for _ in 0..key.wRepeatCount.max(1) {
                            decoder.push(unit, bytes);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => *resized = true,
                _ => {}
            }
        }
        queued = queued.saturating_sub(read);
    }
    Ok(())
}

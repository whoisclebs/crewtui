//! Reads the console on its own thread and turns what it sees into
//! [`Input`] values.
//!
//! With virtual terminal input the console reports every key as the escape
//! sequence a Unix terminal would send, one character to a key event. Those
//! characters are turned back into bytes and go through the same
//! [`Parser`] as on Unix. A resize is noticed from its own record, and by
//! looking at the window size when the wait times out, because the console
//! only reports a change of the screen buffer and not of the window on it.
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
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};

use crate::Parser;
use crate::runtime::Input;
use crate::signal::Signal;
use crate::signals;
use crate::terminal::{Handle, window_size};
use crate::utf16::Utf16Decoder;

/// How long a lone Esc waits to see whether a sequence follows.
pub(crate) const ESCAPE_TIMEOUT_MS: u32 = 50;
/// How long a paste may sit idle before it is given up on.
const PASTE_IDLE_MS: u32 = 1000;
const RECORDS: usize = 128;
/// How often the window size is looked at when nothing else wakes the wait.
const SIZE_POLL_MS: u32 = 250;

/// The reader thread. Dropping it stops the thread and waits for it.
pub(crate) struct InputReader {
    wake: Handle,
    thread: Option<JoinHandle<()>>,
}

impl InputReader {
    /// Starts reading `input`, sending to `tx`. It also passes on the signals
    /// that set `signal_event`, and a change of the size of the window on
    /// `output`.
    pub(crate) fn spawn<M: Send + 'static>(
        input: Handle,
        output: Handle,
        signal_event: Option<Handle>,
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
                        &Handles {
                            input,
                            output,
                            wake,
                            signal_event,
                        },
                        &tx,
                        ESCAPE_TIMEOUT_MS,
                        keyboard_enhancement,
                    );
                }));
                if outcome.is_err() {
                    // Other senders keep the channel open, so a silent death
                    // would leave the loop waiting forever.
                    let _ = tx.send(Input::Failed(io::Error::other("the input thread panicked")));
                }
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
}

impl Drop for InputReader {
    fn drop(&mut self) {
        // SAFETY: the event is open until the end of this function.
        unsafe { SetEvent(self.wake.0) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // SAFETY: closes the event created in `spawn`, once, after the
        // thread that waited on it is gone.
        unsafe { CloseHandle(self.wake.0) };
    }
}

struct Handles {
    input: Handle,
    output: Handle,
    wake: Handle,
    signal_event: Option<Handle>,
}

/// Runs until told to stop or the input fails.
fn read_loop<M>(
    h: &Handles,
    tx: &Sender<Input<M>>,
    escape_timeout_ms: u32,
    keyboard_enhancement: bool,
) {
    let mut parser = Parser::new().with_keyboard_enhancement(keyboard_enhancement);
    let mut decoder = Utf16Decoder::default();
    // When input last arrived. The Esc and paste timeouts count from here.
    let mut last_input = Instant::now();
    let mut size = window_size(h.output).ok();
    let mut records: Vec<INPUT_RECORD> = vec![unsafe_zeroed_record(); RECORDS];
    // The wake event goes first. The wait returns the lowest handle that is
    // set, so a stream of input can't keep it from being noticed.
    let mut handles = vec![h.wake.0, h.input.0];
    if let Some(s) = h.signal_event {
        handles.push(s.0);
    }
    loop {
        let limit = if parser.is_pasting() {
            Some(PASTE_IDLE_MS)
        } else if parser.is_waiting() {
            Some(escape_timeout_ms)
        } else {
            None
        };
        let left = limit.map(|limit| {
            let left = Duration::from_millis(u64::from(limit)).saturating_sub(last_input.elapsed());
            // Round up, so a wake-up just short of the limit doesn't spin.
            left.as_micros()
                .div_ceil(1000)
                .min(u128::from(SIZE_POLL_MS)) as u32
        });
        let timeout = left.map_or(SIZE_POLL_MS, |left| left.min(SIZE_POLL_MS));
        // SAFETY: the pointer and count come from a live vector of handles
        // that stay open while this waits.
        let waited =
            unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), 0, timeout) };
        if waited == WAIT_FAILED {
            let _ = tx.send(Input::Failed(io::Error::last_os_error()));
            return;
        }
        if waited == WAIT_TIMEOUT {
            let expired =
                limit.is_some_and(|l| last_input.elapsed() >= Duration::from_millis(u64::from(l)));
            if expired {
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
            }
        } else {
            match waited - WAIT_OBJECT_0 {
                0 => return,
                1 => {
                    let mut bytes = Vec::new();
                    let mut resized = false;
                    if let Err(e) = read_records(
                        h.input,
                        &mut records,
                        &mut decoder,
                        &mut bytes,
                        &mut resized,
                    ) {
                        let _ = tx.send(Input::Failed(e));
                        return;
                    }
                    if resized {
                        // Forget the size, so the check below reports it.
                        size = None;
                    }
                    if !bytes.is_empty() {
                        last_input = Instant::now();
                        parser.feed(&bytes);
                        while let Some(event) = parser.next_event() {
                            if tx.send(Input::Event(event)).is_err() {
                                return;
                            }
                        }
                    }
                }
                2 => {
                    for signal in signals::take_pending() {
                        if tx.send(Input::Signal(signal)).is_err() {
                            return;
                        }
                    }
                }
                _ => {}
            }
        }
        // A resize record and a change of the window that made no record both
        // show up here.
        if let Ok(now) = window_size(h.output) {
            if size != Some(now) {
                size = Some(now);
                if tx.send(Input::Signal(Signal::Resize)).is_err() {
                    return;
                }
            }
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
        collect(&records[..read as usize], decoder, bytes, resized);
        queued = queued.saturating_sub(read);
    }
    Ok(())
}

/// Turns records into bytes for the parser. Only key presses count. In
/// virtual terminal input mode, Ctrl+Space arrives as a press with no
/// character and no virtual key code, and it is the NUL that Unix sends. A
/// press with no character and a key code is a modifier or a function key on
/// its own, and carries nothing.
fn collect(
    records: &[INPUT_RECORD],
    decoder: &mut Utf16Decoder,
    bytes: &mut Vec<u8>,
    resized: &mut bool,
) {
    for record in records {
        match u32::from(record.EventType) {
            KEY_EVENT => {
                // SAFETY: the event type says which member is valid.
                let key = unsafe { record.Event.KeyEvent };
                // SAFETY: both members of the union are integers.
                let unit = unsafe { key.uChar.UnicodeChar };
                if key.bKeyDown == 0 || (unit == 0 && key.wVirtualKeyCode != 0) {
                    continue;
                }
                for _ in 0..key.wRepeatCount.max(1) {
                    decoder.push(unit, bytes);
                }
            }
            WINDOW_BUFFER_SIZE_EVENT => *resized = true,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Console::KEY_EVENT_RECORD;

    fn key(unit: u16, vk: u16, down: bool, repeat: u16) -> INPUT_RECORD {
        let mut record = unsafe_zeroed_record();
        record.EventType = KEY_EVENT as u16;
        let mut event: KEY_EVENT_RECORD = unsafe { std::mem::zeroed() };
        event.bKeyDown = i32::from(down);
        event.wRepeatCount = repeat;
        event.wVirtualKeyCode = vk;
        event.uChar.UnicodeChar = unit;
        record.Event.KeyEvent = event;
        record
    }

    fn run(records: &[INPUT_RECORD]) -> (Vec<u8>, bool) {
        let mut decoder = Utf16Decoder::default();
        let (mut bytes, mut resized) = (Vec::new(), false);
        collect(records, &mut decoder, &mut bytes, &mut resized);
        (bytes, resized)
    }

    #[test]
    fn key_presses_become_bytes_and_releases_do_not() {
        let (bytes, _) = run(&[
            key(u16::from(b'a'), 0x41, true, 1),
            key(u16::from(b'a'), 0x41, false, 1),
        ]);
        assert_eq!(bytes, b"a");
    }

    #[test]
    fn a_repeat_count_repeats_the_character() {
        let (bytes, _) = run(&[key(u16::from(b'x'), 0x58, true, 3)]);
        assert_eq!(bytes, b"xxx");
        let (bytes, _) = run(&[key(u16::from(b'x'), 0x58, true, 0)]);
        assert_eq!(bytes, b"x");
    }

    #[test]
    fn the_units_of_a_surrogate_pair_are_joined_across_records() {
        // U+1F600, as the console delivers it: one unit per record.
        let (bytes, _) = run(&[key(0xD83D, 0, true, 1), key(0xDE00, 0, true, 1)]);
        assert_eq!(bytes, "\u{1F600}".as_bytes());
    }

    #[test]
    fn ctrl_space_is_a_nul_and_a_bare_modifier_is_nothing() {
        // Ctrl+Space: no character, no key code.
        let (bytes, _) = run(&[key(0, 0, true, 1)]);
        assert_eq!(bytes, [0]);
        // Shift on its own: no character, but a key code.
        let (bytes, _) = run(&[key(0, 0x10, true, 1)]);
        assert!(bytes.is_empty());
    }

    #[test]
    fn a_buffer_size_record_says_the_window_changed() {
        let mut record = unsafe_zeroed_record();
        record.EventType = WINDOW_BUFFER_SIZE_EVENT as u16;
        let (bytes, resized) = run(&[record]);
        assert!(bytes.is_empty());
        assert!(resized);
    }

    #[test]
    fn other_records_are_ignored() {
        let mut record = unsafe_zeroed_record();
        record.EventType = 0x0010; // FOCUS_EVENT
        let (bytes, resized) = run(&[record]);
        assert!(bytes.is_empty() && !resized);
    }
}

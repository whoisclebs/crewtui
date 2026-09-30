//! Raw mode, virtual terminal sequences and the other terminal modes on the
//! Windows console, undone on drop.
//!
//! The console is put in the mode where it reads and writes the same escape
//! sequences a Unix terminal does: virtual terminal processing on output and
//! virtual terminal input on input, both UTF-8. Everything above this file
//! then works as it does on Unix.
#![allow(unsafe_code)]

use std::io::{self, Write};
use std::marker::PhantomData;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::thread::{self, ThreadId};

use windows_sys::Win32::Foundation::{GetLastError, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::WriteFile;
use windows_sys::Win32::System::Console::{
    CONSOLE_MODE, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_ECHO_INPUT,
    ENABLE_EXTENDED_FLAGS, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_QUICK_EDIT_MODE, ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING,
    ENABLE_WINDOW_INPUT, GetConsoleCP, GetConsoleMode, GetConsoleOutputCP,
    GetConsoleScreenBufferInfo, GetStdHandle, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    SetConsoleCP, SetConsoleMode, SetConsoleOutputCP,
};

use crate::options::{TerminalOptions, disable_sequence, enable_sequence};

const UTF8: u32 = 65001;

/// A console handle. It is only used for calls that take a handle, from any
/// thread, so it can be shared.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Handle(pub(crate) HANDLE);

// SAFETY: a console handle is an identifier the system checks on each call.
unsafe impl Send for Handle {}
// SAFETY: as above.
unsafe impl Sync for Handle {}

fn last_error() -> io::Error {
    // SAFETY: reads the calling thread's last error.
    io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
}

pub(crate) fn std_handle(which: STD_HANDLE) -> io::Result<Handle> {
    // SAFETY: takes one of the three constants and has no other input.
    let h = unsafe { GetStdHandle(which) };
    if h.is_null() || h == INVALID_HANDLE_VALUE {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "there is no console handle",
        ));
    }
    Ok(Handle(h))
}

fn get_mode(h: Handle) -> io::Result<CONSOLE_MODE> {
    let mut mode = 0;
    // SAFETY: `mode` is valid for a write.
    if unsafe { GetConsoleMode(h.0, &mut mode) } == 0 {
        return Err(last_error());
    }
    Ok(mode)
}

fn set_mode(h: Handle, mode: CONSOLE_MODE) -> io::Result<()> {
    // SAFETY: only reads its arguments.
    if unsafe { SetConsoleMode(h.0, mode) } == 0 {
        return Err(last_error());
    }
    Ok(())
}

fn write_all(h: Handle, mut buf: &[u8]) -> io::Result<()> {
    while !buf.is_empty() {
        let mut written = 0u32;
        let len = buf.len().min(u32::MAX as usize) as u32;
        // SAFETY: the pointer and length come from a live slice, and
        // `written` is valid for a write.
        let ok = unsafe { WriteFile(h.0, buf.as_ptr(), len, &mut written, std::ptr::null_mut()) };
        if ok == 0 {
            return Err(last_error());
        }
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        buf = &buf[written as usize..];
    }
    Ok(())
}

/// The console window's size as `(columns, rows)`.
pub(crate) fn window_size(h: Handle) -> io::Result<(u16, u16)> {
    // SAFETY: an all-zero struct of integers is valid.
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is valid for a write.
    if unsafe { GetConsoleScreenBufferInfo(h.0, &mut info) } == 0 {
        return Err(last_error());
    }
    let w = i32::from(info.srWindow.Right) - i32::from(info.srWindow.Left) + 1;
    let h = i32::from(info.srWindow.Bottom) - i32::from(info.srWindow.Top) + 1;
    Ok((
        w.clamp(0, i32::from(u16::MAX)) as u16,
        h.clamp(0, i32::from(u16::MAX)) as u16,
    ))
}

struct State {
    input: Handle,
    output: Handle,
    original_input_mode: CONSOLE_MODE,
    original_output_mode: CONSOLE_MODE,
    original_input_cp: u32,
    original_output_cp: u32,
    options: TerminalOptions,
    restored: AtomicBool,
    owner: ThreadId,
}

impl State {
    /// Puts the console in the mode the loop needs and writes the enable
    /// sequences. The modes are computed from the ones the console had.
    fn set_modes(&self) -> io::Result<()> {
        let input = (self.original_input_mode
            & !(ENABLE_ECHO_INPUT
                | ENABLE_LINE_INPUT
                | ENABLE_PROCESSED_INPUT
                | ENABLE_QUICK_EDIT_MODE))
            | ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_EXTENDED_FLAGS
            | ENABLE_WINDOW_INPUT;
        set_mode(self.input, input)?;
        let output = self.original_output_mode
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
        // Not returning to the start of the line on a newline is what the
        // renderer expects, and an older console may not have it.
        if set_mode(self.output, output | DISABLE_NEWLINE_AUTO_RETURN).is_err() {
            set_mode(self.output, output).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("the console does not do virtual terminal sequences: {e}"),
                )
            })?;
        }
        // SAFETY: only reads its argument. A failure leaves the code page as
        // it was, and the text may then not show properly.
        unsafe {
            SetConsoleOutputCP(UTF8);
            SetConsoleCP(UTF8);
        }
        Ok(())
    }

    fn activate(self: &Arc<Self>) -> io::Result<()> {
        // From the first change on, `restore` has something to undo, so a
        // failure half way leaves the console as it was found.
        self.restored.store(false, Ordering::SeqCst);
        let done = self
            .set_modes()
            .and_then(|()| write_all(self.output, &enable_sequence(&self.options)));
        if let Err(e) = done {
            let _ = self.restore();
            return Err(e);
        }
        let mut active = ACTIVE.lock().unwrap_or_else(PoisonError::into_inner);
        if !active.iter().any(|s| Arc::ptr_eq(s, self)) {
            active.push(self.clone());
        }
        Ok(())
    }

    fn reapply(&self) -> io::Result<()> {
        self.set_modes()?;
        let mut sequence = Vec::new();
        if self.options.keyboard_enhancement {
            // What an earlier entry pushed is still there, and `restore` pops
            // once. Popping first, from an empty stack too, leaves one.
            sequence.extend_from_slice(b"\x1b[<u");
        }
        sequence.extend_from_slice(&enable_sequence(&self.options));
        write_all(self.output, &sequence)
    }

    /// Undoes everything `activate` did. Only the first call does any work.
    fn restore(self: &Arc<Self>) -> io::Result<()> {
        if self.restored.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let written = write_all(self.output, &disable_sequence(&self.options));
        // SAFETY: only read their arguments.
        unsafe {
            SetConsoleOutputCP(self.original_output_cp);
            SetConsoleCP(self.original_input_cp);
        }
        let input = set_mode(self.input, self.original_input_mode);
        let output = set_mode(self.output, self.original_output_mode);
        ACTIVE
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|s| !Arc::ptr_eq(s, self));
        written.and(input).and(output)
    }
}

/// Terminals currently in raw mode, so the panic hook can find them.
static ACTIVE: Mutex<Vec<Arc<State>>> = Mutex::new(Vec::new());
static HOOK: Once = Once::new();

/// Chains a panic hook that restores the terminals entered by the
/// panicking thread before the panic message is printed, so the message
/// lands on the normal screen. Terminals belonging to other threads are left
/// alone.
fn install_panic_hook() {
    HOOK.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let me = thread::current().id();
            let mine: Vec<Arc<State>> = ACTIVE
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .filter(|s| s.owner == me)
                .cloned()
                .collect();
            for state in mine {
                let _ = state.restore();
            }
            previous(info);
        }));
    });
}

/// A terminal in raw mode. Dropping it restores the terminal.
///
/// [`Program`](crate::Program) enters and restores the terminal for you, and
/// is what most apps use. `Terminal` is the piece underneath it, for an app
/// that runs a loop of its own.
///
/// On Windows this is the console, in virtual terminal mode, which needs
/// Windows 10 or later. Raw mode is what makes key presses arrive one at a
/// time, unechoed and without signal generation; Ctrl+C is a key, like on
/// Unix. Everything that was turned on is turned off again when the value
/// is dropped, whether the app returned normally, returned an error, or
/// panicked with unwinding. A panic hook covers the case where it aborts
/// instead.
///
/// `std::process::exit` skips destructors. Call [`Terminal::restore`] first
/// if you need to exit that way.
///
/// A `Terminal` isn't `Send`: it belongs to the thread that entered.
///
/// The terminal is also a [`Write`] sink for output to the console.
pub struct Terminal {
    state: Arc<State>,
    /// Ties the value to the thread that entered, which is the thread the
    /// panic hook restores for.
    _not_send: PhantomData<*const ()>,
    pub(crate) skip_continue: bool,
}

impl Terminal {
    /// Puts the console in raw mode with the modes in `options`. Standard
    /// input and standard output must both be consoles.
    pub fn enter(options: TerminalOptions) -> io::Result<Terminal> {
        let input = std_handle(STD_INPUT_HANDLE)?;
        let output = std_handle(STD_OUTPUT_HANDLE)?;
        let not_a_console = |name: &str| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{name} is not a terminal"),
            )
        };
        let original_input_mode = get_mode(input).map_err(|_| not_a_console("stdin"))?;
        let original_output_mode = get_mode(output).map_err(|_| not_a_console("stdout"))?;
        let state = Arc::new(State {
            input,
            output,
            original_input_mode,
            original_output_mode,
            // SAFETY: reads the console's code pages and has no input.
            original_input_cp: unsafe { GetConsoleCP() },
            original_output_cp: unsafe { GetConsoleOutputCP() },
            options,
            restored: AtomicBool::new(true),
            owner: thread::current().id(),
        });
        install_panic_hook();
        state.activate().inspect_err(|_| {
            let _ = state.restore();
        })?;
        Ok(Terminal {
            state,
            _not_send: PhantomData,
            skip_continue: false,
        })
    }

    /// Whether entering left the cursor hidden.
    pub(crate) fn cursor_hidden(&self) -> bool {
        self.state.options.hide_cursor
    }

    /// The console window's size as `(columns, rows)`.
    pub fn size(&self) -> io::Result<(u16, u16)> {
        window_size(self.state.output)
    }

    /// Restores the terminal now instead of on drop. Calling it again, or
    /// dropping afterwards, does nothing.
    pub fn restore(&mut self) -> io::Result<()> {
        self.state.restore()
    }

    /// Goes back to raw mode and the requested screen modes. Use it after
    /// the terminal was restored, for instance by the panic hook when the
    /// panic was caught, or when something else changed the console. It is
    /// safe to call when nothing needs redoing. The next frame has to repaint
    /// everything, so call `Renderer::invalidate` too.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.state.restored.load(Ordering::SeqCst) {
            self.state.activate()
        } else {
            self.state.reapply()
        }
    }

    /// The handle the input reader waits on.
    pub(crate) fn input_handle(&self) -> Handle {
        self.state.input
    }

    /// The handle the size of the window is read from.
    pub(crate) fn output_handle(&self) -> Handle {
        self.state.output
    }
}

impl Write for Terminal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_all(self.state.output, buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.state.restore();
    }
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal")
            .field("options", &self.state.options)
            .field("restored", &self.state.restored.load(Ordering::SeqCst))
            .finish()
    }
}

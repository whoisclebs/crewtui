//! Scenarios that run as a child process on a pty, and the tests that play
//! them out.
//!
//! The parent test calls `Pty::spawn_self("pty_children::child_entry", mode)`,
//! which reruns this test executable with only `child_entry` selected and
//! `CREWTUI_PTY_CHILD` set. When the variable is not set, as in a normal
//! test run, `child_entry` does nothing and passes.

use std::io::{self, Write};

use crate::testing::CHILD_MODE;
use crate::{
    App, Cmd, Event, Frame, KeyCode, Modifiers, Program, Style, Terminal, TerminalOptions,
};

/// A small app that misbehaves on request.
struct Probe {
    mode: String,
    keys: u32,
}

impl App for Probe {
    type Message = crate::KeyEvent;

    fn event(&self, event: Event) -> Option<Self::Message> {
        match event {
            Event::Key(k) => Some(k),
            _ => None,
        }
    }

    fn update(&mut self, key: Self::Message) -> Cmd<Self::Message> {
        self.keys += 1;
        let quit = key.code == KeyCode::Char('q')
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(Modifiers::CTRL));
        if quit {
            return Cmd::quit();
        }
        if self.mode == "panic_update" && key.code == KeyCode::Char('p') {
            panic!("boom in update");
        }
        Cmd::none()
    }

    fn view(&self, frame: &mut Frame<'_>) {
        if self.mode == "panic_view" && self.keys > 0 {
            panic!("boom in view");
        }
        frame.buffer_mut().set_string(0, 0, "ready", Style::new());
    }
}

const ALL_MODES: TerminalOptions = TerminalOptions {
    alternate_screen: true,
    hide_cursor: true,
    mouse: true,
    focus_events: true,
    bracketed_paste: true,
};

fn run_probe(mode: &str, options: TerminalOptions) -> io::Result<()> {
    let app = Probe {
        mode: mode.to_string(),
        keys: 0,
    };
    Program::new(app)
        .terminal_options(options)
        .run()
        .map(|_| ())
}

/// Enters the terminal directly and returns early with `?`.
fn early_return() -> io::Result<()> {
    let mut terminal = Terminal::enter(TerminalOptions::default())?;
    terminal.write_all(b"ready")?;
    Err(io::Error::other("stopped early"))?;
    Ok(())
}

/// Enters the terminal, forgets the guard and panics, so nothing but the
/// panic hook can put the terminal back.
fn leak_and_panic() -> io::Result<()> {
    let mut terminal = Terminal::enter(TerminalOptions::default())?;
    terminal.write_all(b"ready")?;
    std::mem::forget(terminal);
    panic!("boom with the guard leaked");
}

/// The entry point of the child. It plays the scenario and exits the
/// process, except for panics, which unwind out of it like in any test.
#[test]
fn child_entry() {
    let Ok(mode) = std::env::var(CHILD_MODE) else {
        return;
    };
    let result = match mode.as_str() {
        "hello" => {
            println!("hello from the child");
            Ok(())
        }
        "early_return" => early_return(),
        "leak_and_panic" => leak_and_panic(),
        "all_modes" => run_probe("normal", ALL_MODES),
        "agent" => crate::agent_example::run(crate::agent_example::Pace::INSTANT, false),
        "agent_stress" => crate::agent_example::run(crate::agent_example::Pace::INSTANT, true),
        other => run_probe(other, TerminalOptions::default()),
    };
    match result {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            // The terminal is restored by now, so this is plain output. If
            // it is gone altogether the write fails, and that is fine.
            let _ = writeln!(io::stdout(), "error: {e}");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::testing::{Pty, kill, same, wait_timeout, write_fd};
    use std::os::unix::process::ExitStatusExt;
    use std::process::Child;
    use std::time::Duration;

    const LIMIT: Duration = Duration::from_secs(30);
    /// What a default `Terminal` writes to leave: paste off, colors reset,
    /// cursor shown, alternate screen left.
    const LEAVE: &[u8] = b"\x1b[?2004l\x1b[0m\x1b[?25h\x1b[?1049l";
    const LEAVE_ALL: &[u8] =
        b"\x1b[?2004l\x1b[?1004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[?1049l";

    /// A child that is running its scenario on a pty.
    struct Scenario {
        pty: Pty,
        child: Child,
        original: libc::termios,
        seen: Vec<u8>,
    }

    impl Scenario {
        /// Starts `mode` and waits until it has drawn, which means the
        /// terminal is raw.
        fn start(mode: &str) -> Scenario {
            Scenario::start_with(mode, true)
        }

        /// Like `start`, for a scenario that leaves on its own right after
        /// drawing, so the terminal is already restored by the time the
        /// parent could look. `expect_raw` false checks that it entered
        /// through the enable sequence instead.
        fn start_with(mode: &str, expect_raw: bool) -> Scenario {
            Scenario::start_sized(mode, expect_raw, 40, 5)
        }

        /// Like `start_with`, on a terminal of `columns` by `rows`.
        fn start_sized(mode: &str, expect_raw: bool, columns: u16, rows: u16) -> Scenario {
            let pty = Pty::open();
            pty.set_size(columns, rows);
            let original = pty.termios();
            let child = pty.spawn_self("pty_children::child_entry", mode).unwrap();
            let mut scenario = Scenario {
                pty,
                child,
                original,
                seen: Vec::new(),
            };
            let up = scenario.pty.read_until(b"ready", LIMIT, &mut scenario.seen);
            assert!(
                up,
                "the child never drew: {:?}",
                String::from_utf8_lossy(&scenario.seen)
            );
            if expect_raw {
                assert!(
                    scenario.pty.is_raw(),
                    "the terminal isn't raw while the child runs"
                );
            } else {
                let enter = b"\x1b[?1049h";
                assert!(
                    scenario.seen.windows(enter.len()).any(|w| w == enter),
                    "the child never entered the alternate screen"
                );
            }
            scenario
        }

        fn type_bytes(&self, bytes: &[u8]) {
            write_fd(self.pty.master, bytes);
        }

        fn signal(&self, sig: libc::c_int) {
            kill(&self.child, sig);
        }

        /// Waits for the child to exit and collects everything it wrote.
        fn finish(&mut self) -> std::process::ExitStatus {
            let status = wait_timeout(&mut self.child, LIMIT).unwrap_or_else(|| {
                let _ = self.child.kill();
                panic!(
                    "the child never exited: {:?}",
                    String::from_utf8_lossy(&self.seen)
                );
            });
            self.seen.extend(self.pty.output());
            status
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.seen).into_owned()
        }

        /// The terminal is back to how it was, and the child said so.
        fn assert_restored(&self, leave: &[u8]) {
            assert!(
                same(&self.pty.termios(), &self.original),
                "termios was not restored"
            );
            assert!(
                self.seen.windows(leave.len()).any(|w| w == leave),
                "the leave sequence is missing from {:?}",
                self.text()
            );
        }

        fn position(&self, needle: &str) -> Option<usize> {
            self.text().find(needle)
        }
    }

    #[test]
    fn a_normal_exit_restores_everything() {
        let mut s = Scenario::start("normal");
        s.type_bytes(b"q");
        let status = s.finish();
        assert_eq!(status.code(), Some(0), "{}", s.text());
        s.assert_restored(LEAVE);
    }

    #[test]
    fn a_normal_exit_turns_off_mouse_focus_and_paste_reporting_too() {
        let mut s = Scenario::start("all_modes");
        // The modes are on while the program runs.
        let on = b"\x1b[?1000h";
        assert!(s.seen.windows(on.len()).any(|w| w == on), "{}", s.text());
        s.type_bytes(b"q");
        assert_eq!(s.finish().code(), Some(0));
        s.assert_restored(LEAVE_ALL);
    }

    #[test]
    fn ctrl_c_in_raw_mode_is_a_key_the_app_can_quit_on_not_a_signal() {
        let mut s = Scenario::start("normal");
        s.type_bytes(&[0x03]);
        let status = s.finish();
        assert_eq!(status.code(), Some(0), "{status:?} {}", s.text());
        assert_eq!(status.signal(), None);
        s.assert_restored(LEAVE);
    }

    #[test]
    fn returning_an_error_early_with_a_question_mark_restores_the_terminal() {
        let mut s = Scenario::start_with("early_return", false);
        let status = s.finish();
        assert_eq!(status.code(), Some(2), "{}", s.text());
        assert!(s.text().contains("error: stopped early"));
        s.assert_restored(LEAVE);
    }

    #[test]
    fn a_panic_in_update_restores_the_terminal_before_the_message_prints() {
        let mut s = Scenario::start("panic_update");
        s.type_bytes(b"p");
        let status = s.finish();
        assert_eq!(status.code(), Some(101), "{}", s.text());
        s.assert_restored(LEAVE);
        let left = s
            .position("\x1b[?1049l")
            .expect("never left the alternate screen");
        let message = s
            .position("boom in update")
            .expect("the panic message was lost");
        assert!(
            left < message,
            "the message was printed on the alternate screen"
        );
    }

    #[test]
    fn a_panic_in_view_restores_the_terminal_before_the_message_prints() {
        let mut s = Scenario::start("panic_view");
        s.type_bytes(b"x");
        let status = s.finish();
        assert_eq!(status.code(), Some(101), "{}", s.text());
        s.assert_restored(LEAVE);
        let left = s
            .position("\x1b[?1049l")
            .expect("never left the alternate screen");
        let message = s
            .position("boom in view")
            .expect("the panic message was lost");
        assert!(left < message);
    }

    #[test]
    fn a_panic_with_the_guard_leaked_is_still_restored_by_the_hook() {
        // Stands in for `panic = "abort"`, where no destructor runs.
        let mut s = Scenario::start_with("leak_and_panic", false);
        let status = s.finish();
        assert_eq!(status.code(), Some(101), "{}", s.text());
        s.assert_restored(LEAVE);
        let left = s
            .position("\x1b[?1049l")
            .expect("never left the alternate screen");
        let message = s
            .position("boom with the guard leaked")
            .expect("the panic message was lost");
        assert!(left < message);
    }

    fn stopped_by_signal(mode: &str, sig: libc::c_int, expect: &str) {
        let mut s = Scenario::start(mode);
        s.signal(sig);
        let status = s.finish();
        assert_eq!(status.code(), Some(2), "{status:?} {}", s.text());
        assert!(s.text().contains(expect), "{}", s.text());
        s.assert_restored(LEAVE);
    }

    #[test]
    fn sigterm_from_outside_ends_the_run_and_restores_the_terminal() {
        stopped_by_signal("normal", libc::SIGTERM, "terminated (SIGTERM)");
    }

    #[test]
    fn sigint_from_outside_ends_the_run_and_restores_the_terminal() {
        stopped_by_signal("normal", libc::SIGINT, "interrupted (SIGINT)");
    }

    #[test]
    fn sighup_from_outside_ends_the_run_and_restores_the_terminal() {
        stopped_by_signal("normal", libc::SIGHUP, "hung up (SIGHUP)");
    }

    /// Feeds what the child writes to `screen` until some row holds `text`.
    fn wait_for_row(s: &mut Scenario, screen: &mut crate::testing::Screen, text: &str) {
        let deadline = std::time::Instant::now() + LIMIT;
        loop {
            let out = s.pty.output_within(std::time::Duration::from_millis(500));
            s.seen.extend_from_slice(&out);
            screen.feed(&out);
            if (0..30).any(|y| screen.row(y).contains(text)) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "never saw {text:?} on the screen: {:?}",
                (0..30).map(|y| screen.row(y)).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn the_agent_example_answers_a_request_and_quits_cleanly_on_ctrl_c() {
        let mut s = Scenario::start_sized("agent", true, 100, 30);
        let mut screen = crate::testing::Screen::new(100, 30);
        // What was drawn before `start_sized` returned.
        screen.feed(&s.seen);
        wait_for_row(&mut s, &mut screen, "crewtui agent");
        s.type_bytes(b"hello there\r");
        wait_for_row(&mut s, &mut screen, "you \u{25b8} hello there");
        // The worker thread streams the whole answer, tools included.
        wait_for_row(
            &mut s,
            &mut screen,
            "behavior you're after is already there",
        );
        wait_for_row(&mut s, &mut screen, "\u{2713} run_tests");
        // The status line, not a word that happens to be in the answer.
        wait_for_row(&mut s, &mut screen, "ready \u{b7} Enter sends");
        // Ctrl+C is a key in raw mode, and the app quits on it.
        s.type_bytes(&[0x03]);
        let status = s.finish();
        assert_eq!(status.code(), Some(0), "{status:?}");
        assert!(
            s.seen
                .windows(b"\x1b[?1049l".len())
                .any(|w| w == b"\x1b[?1049l"),
            "the alternate screen was never left"
        );
        assert!(
            same(&s.pty.termios(), &s.original),
            "termios was not restored"
        );
    }

    #[test]
    fn the_agent_example_survives_its_stress_mode_and_quits_cleanly() {
        let mut s = Scenario::start_sized("agent_stress", true, 100, 30);
        let mut screen = crate::testing::Screen::new(100, 30);
        screen.feed(&s.seen);
        wait_for_row(&mut s, &mut screen, "crewtui agent");
        // A request arrives by itself every second, on top of 20,000 lines.
        wait_for_row(&mut s, &mut screen, "you \u{25b8} stress request 1");
        wait_for_row(
            &mut s,
            &mut screen,
            "behavior you're after is already there",
        );
        s.type_bytes(&[0x03]);
        let status = s.finish();
        assert_eq!(status.code(), Some(0), "{status:?}");
        assert!(
            same(&s.pty.termios(), &s.original),
            "termios was not restored"
        );
    }

    #[test]
    fn the_terminal_going_away_ends_the_run_instead_of_hanging() {
        let mut s = Scenario::start("normal");
        s.pty.close_master();
        let status = wait_timeout(&mut s.child, LIMIT);
        if status.is_none() {
            let _ = s.child.kill();
        }
        // There is no terminal left to restore; the point is that the
        // process stops, and stops through the run's error path.
        assert_eq!(status.and_then(|st| st.code()), Some(2));
    }
}

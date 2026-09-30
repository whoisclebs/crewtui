//! Tests for the reference app in `examples/agent.rs`, driving its `update`
//! and `view` without a terminal or threads. The threaded run on a pty is in
//! `pty_children`.

use crate::agent_example::{Agent, Msg, Pace, Status};
use crate::testing::Screen;
use crate::{
    App, Buffer, Event, Frame, KeyCode, KeyEvent, Modifiers, MouseEvent, MouseKind, Rect, Renderer,
};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: Modifiers::NONE,
    }
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent {
        code: KeyCode::Char(c),
        modifiers: Modifiers::CTRL,
    }
}

fn press(agent: &mut Agent, event: Event) -> String {
    match agent.event(event) {
        Some(msg) => format!("{:?}", agent.update(msg)),
        None => "no message".to_owned(),
    }
}

fn type_text(agent: &mut Agent, text: &str) {
    for c in text.chars() {
        press(agent, Event::Key(key(KeyCode::Char(c))));
    }
}

fn agent() -> Agent {
    Agent::new(Pace::INSTANT)
}

/// The screen after drawing `agent` at `w` by `h`.
fn screen(agent: &Agent, w: u16, h: u16) -> Screen {
    let mut renderer = Renderer::new(w, h);
    let mut screen = Screen::new(w, h);
    screen.feed(renderer.draw_frame(|f| agent.view(f)));
    screen
}

fn rows(screen: &Screen, h: u16) -> Vec<String> {
    (0..usize::from(h)).map(|y| screen.row(y)).collect()
}

fn history_text(agent: &Agent) -> String {
    (0..agent.history.len())
        .filter_map(|i| agent.history.entry(i))
        .flat_map(|t| t.lines.iter())
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn enter_sends_the_request_and_starts_the_agent() {
    let mut a = agent();
    type_text(&mut a, "fix the bug");
    assert_eq!(a.input.text(), "fix the bug");
    let cmd = press(&mut a, Event::Key(key(KeyCode::Enter)));
    assert!(cmd.contains("Cmd::batch(2 commands)"), "{cmd}");
    assert_eq!(a.status, Status::Thinking);
    assert!(a.input.is_empty());
    assert!(history_text(&a).contains("you ▸ fix the bug"));
}

#[test]
fn enter_on_an_empty_input_or_while_busy_does_nothing() {
    let mut a = agent();
    assert_eq!(
        press(&mut a, Event::Key(key(KeyCode::Enter))),
        "Cmd::none()"
    );
    type_text(&mut a, "   ");
    assert_eq!(
        press(&mut a, Event::Key(key(KeyCode::Enter))),
        "Cmd::none()"
    );
    assert_eq!(a.status, Status::Ready);

    type_text(&mut a, "one");
    press(&mut a, Event::Key(key(KeyCode::Enter)));
    let before = a.history.len();
    type_text(&mut a, "two");
    assert_eq!(
        press(&mut a, Event::Key(key(KeyCode::Enter))),
        "Cmd::none()"
    );
    assert_eq!(a.history.len(), before);
    assert_eq!(a.input.text(), "two", "what was typed is kept");
}

#[test]
fn tokens_tools_and_the_end_of_the_answer_move_the_status() {
    let mut a = agent();
    a.update(Msg::Submit("go".into()));
    a.update(Msg::Token("Hello ".into()));
    a.update(Msg::Token("there\n".into()));
    a.update(Msg::ToolStarted("read_file x".into()));
    assert_eq!(a.status, Status::Tool("read_file x".into()));
    a.update(Msg::ToolFinished("read_file x".into()));
    assert_eq!(a.status, Status::Thinking);
    assert_eq!(a.tools, ["✓ read_file x"]);
    a.update(Msg::Token("done".into()));
    a.update(Msg::Finished);
    assert_eq!(a.status, Status::Ready);
    let text = history_text(&a);
    assert!(text.contains("agent ▸ Hello there"), "{text}");
    assert!(text.contains("⚙ read_file x"), "{text}");
    assert!(text.ends_with("agent ▸ done"), "{text}");
}

#[test]
fn the_spinner_only_keeps_ticking_while_busy() {
    let mut a = agent();
    assert_eq!(format!("{:?}", a.update(Msg::Spin)), "Cmd::none()");
    a.update(Msg::Submit("go".into()));
    assert!(format!("{:?}", a.update(Msg::Spin)).starts_with("Cmd::after("));
    a.update(Msg::Finished);
    assert_eq!(format!("{:?}", a.update(Msg::Spin)), "Cmd::none()");
}

#[test]
fn ctrl_c_quits_and_ctrl_l_repaints() {
    let mut a = agent();
    assert_eq!(press(&mut a, Event::Key(ctrl('c'))), "Cmd::quit()");
    assert_eq!(press(&mut a, Event::Key(ctrl('l'))), "Cmd::repaint()");
    // Other control keys still reach the input.
    type_text(&mut a, "abc");
    press(&mut a, Event::Key(ctrl('a')));
    assert_eq!(a.input.cursor(), 0);
}

#[test]
fn a_paste_goes_into_the_input_as_text() {
    let mut a = agent();
    press(&mut a, Event::Paste("one\ntwo".into()));
    assert_eq!(a.input.text(), "onetwo");
}

#[test]
fn the_wheel_scrolls_the_history_and_ignores_the_rest_of_the_mouse() {
    let mut a = agent().with_history(200);
    screen(&a, 100, 30);
    let wheel = |kind| {
        Event::Mouse(MouseEvent {
            kind,
            column: 3,
            row: 3,
            modifiers: Modifiers::NONE,
        })
    };
    press(&mut a, wheel(MouseKind::ScrollUp));
    assert!(!a.history.is_following());
    press(&mut a, wheel(MouseKind::ScrollDown));
    assert!(a.history.is_following());
    assert_eq!(press(&mut a, wheel(MouseKind::Moved)), "no message");
    assert_eq!(press(&mut a, Event::FocusGained), "no message");
}

#[test]
fn a_reader_who_scrolled_up_is_not_moved_by_tokens_or_background_notes() {
    let mut a = agent().with_history(300);
    a.update(Msg::Submit("go".into()));
    screen(&a, 100, 30);
    press(&mut a, Event::Key(key(KeyCode::PageUp)));
    press(&mut a, Event::Key(key(KeyCode::PageUp)));
    let before = rows(&screen(&a, 100, 30), 30);
    assert!(before.iter().any(|r| r.contains("earlier output")));
    for i in 0..40 {
        a.update(Msg::Token(format!("token{i} ")));
        a.update(Msg::Clock);
    }
    a.update(Msg::ToolStarted("run_tests".into()));
    let after = rows(&screen(&a, 100, 30), 30);
    // The history rows are the same; only the header's clock and the status
    // line changed.
    assert_eq!(before[2..25], after[2..25]);
    assert!(after[26].contains("scrolled up"), "{:?}", after[26]);
}

#[test]
fn the_view_shows_every_region() {
    let mut a = agent();
    a.update(Msg::Submit("hello".into()));
    a.update(Msg::ToolStarted("read_file src/lib.rs".into()));
    a.update(Msg::ToolFinished("read_file src/lib.rs".into()));
    a.update(Msg::Resize(100, 30));
    type_text(&mut a, "draft");
    let rows = rows(&screen(&a, 100, 30), 30);
    let all = rows.join("\n");
    assert!(
        rows[0].contains("crewtui agent") && rows[0].contains("100x30"),
        "{}",
        rows[0]
    );
    assert!(all.contains("history"), "{all}");
    assert!(all.contains("tools"), "{all}");
    assert!(all.contains("✓ read_file src/lib.rs"), "{all}");
    assert!(all.contains("you ▸ hello"), "{all}");
    assert!(rows[26].contains("thinking"), "{:?}", rows[26]);
    assert!(rows[28].contains("draft"), "{:?}", rows[28]);
}

#[test]
fn the_side_panel_only_shows_when_there_is_room() {
    let a = agent();
    assert!(rows(&screen(&a, 100, 12), 12).join("\n").contains("tools"));
    assert!(!rows(&screen(&a, 60, 12), 12).join("\n").contains("tools"));
}

#[test]
fn the_view_draws_at_any_size_without_panicking() {
    let mut a = agent().with_history(50);
    a.update(Msg::Submit("hello".into()));
    a.update(Msg::ToolStarted("x".into()));
    a.update(Msg::ToolFinished("x".into()));
    type_text(&mut a, "some words in the input box");
    for w in 0..=14u16 {
        for h in 0..=8u16 {
            let mut buf = Buffer::new(Rect::new(0, 0, w, h));
            a.view(&mut Frame::new(&mut buf));
        }
    }
    for (w, h) in [(1, 1), (3, 40), (200, 2), (39, 5), (60, 9), (300, 100)] {
        let mut buf = Buffer::new(Rect::new(0, 0, w, h));
        a.view(&mut Frame::new(&mut buf));
    }
}

#[test]
fn the_cursor_is_in_the_input_box() {
    let mut a = agent();
    type_text(&mut a, "abc");
    let mut renderer = Renderer::new(100, 30);
    let mut screen = Screen::new(100, 30);
    screen.feed(b"\x1b[?25l");
    screen.feed(renderer.draw_frame(|f| a.view(f)));
    let ((x, y), shown) = screen.cursor();
    assert!(shown);
    // Inside the bordered input at the bottom, after the three letters.
    assert_eq!((x, y), (1 + 3, 28));
}

#[test]
fn a_request_from_a_driver_while_the_agent_is_busy_is_dropped() {
    let mut a = agent();
    a.update(Msg::Submit("first".into()));
    let len = a.history.len();
    assert_eq!(
        format!("{:?}", a.update(Msg::Submit("second".into()))),
        "Cmd::none()"
    );
    assert_eq!(a.history.len(), len);
    a.update(Msg::Finished);
    assert!(format!("{:?}", a.update(Msg::Submit("second".into()))).starts_with("Cmd::batch"));
}

#[test]
fn every_fifth_second_a_background_note_lands_in_the_history() {
    let mut a = agent();
    let before = a.history.len();
    for _ in 0..4 {
        a.update(Msg::Clock);
    }
    assert_eq!(a.history.len(), before);
    a.update(Msg::Clock);
    assert_eq!(a.history.len(), before + 1);
    assert!(history_text(&a).contains("background: indexed"));
    assert_eq!(a.uptime, 5);
}

#[test]
fn a_background_note_in_the_middle_of_an_answer_does_not_swallow_the_tokens() {
    let mut a = agent();
    a.update(Msg::Submit("go".into()));
    a.update(Msg::Token("Hello ".into()));
    for _ in 0..5 {
        a.update(Msg::Clock);
    }
    a.update(Msg::Token("world".into()));
    let text = history_text(&a);
    assert!(text.contains("indexed 105 files\n"), "{text:?}");
    assert!(
        !text.contains("fileswworld") && !text.contains("filesworld"),
        "{text:?}"
    );
    let last = a.history.entry(a.history.len() - 1).unwrap();
    assert_eq!(last.lines[0].spans.last().unwrap().content, "world");
    assert_ne!(
        last.lines[0].spans.last().unwrap().style,
        crate::agent_example::dim(),
        "the answer picked up the note's dim style"
    );
}

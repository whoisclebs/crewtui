//! Timings for the paths a frame goes through: diffing two buffers, drawing
//! a typical layout, and drawing a long transcript.
//!
//! Run with `cargo bench`. There is no harness: each case is run for a while
//! to warm up, then sampled, and the table shows the median and the fastest
//! sample per frame, and the bytes a frame wrote to the terminal on average.

use std::hint::black_box;
use std::time::{Duration, Instant};

use crewtui::widgets::{
    Block, History, HistoryState, Input, InputState, List, ListState, Paragraph, Progress,
    Scrollbar, Wrap,
};
use crewtui::{Buffer, Color, Constraint, Layout, Rect, Renderer, Style};

const WIDTH: u16 = 200;
const HEIGHT: u16 = 60;

/// One case: runs a frame and returns how many bytes it wrote.
struct Case {
    name: String,
    frame: Box<dyn FnMut() -> usize>,
}

fn case(name: impl Into<String>, frame: impl FnMut() -> usize + 'static) -> Case {
    Case {
        name: name.into(),
        frame: Box::new(frame),
    }
}

struct Result {
    name: String,
    median: Duration,
    fastest: Duration,
    bytes: usize,
}

fn measure(mut case: Case) -> Result {
    // Warm up, and find how many frames make a sample last a few
    // milliseconds, so the clock's resolution doesn't matter.
    for _ in 0..10 {
        black_box((case.frame)());
    }
    let mut per_sample = 1usize;
    loop {
        let start = Instant::now();
        for _ in 0..per_sample {
            black_box((case.frame)());
        }
        if start.elapsed() >= Duration::from_millis(5) || per_sample >= 1 << 20 {
            break;
        }
        per_sample *= 2;
    }
    let mut samples = Vec::new();
    let mut all_bytes = 0;
    for _ in 0..25 {
        let start = Instant::now();
        for _ in 0..per_sample {
            all_bytes += black_box((case.frame)());
        }
        samples.push(start.elapsed() / per_sample as u32);
    }
    let bytes = all_bytes / (25 * per_sample);
    samples.sort();
    Result {
        name: case.name,
        median: samples[samples.len() / 2],
        fastest: samples[0],
        bytes,
    }
}

fn print(title: &str, results: &[Result]) {
    println!("\n{title}");
    println!(
        "{:<44} {:>12} {:>12} {:>10}",
        "case", "median", "fastest", "bytes"
    );
    for r in results {
        println!(
            "{:<44} {:>12} {:>12} {:>10}",
            r.name,
            format!("{:.1?}", r.median),
            format!("{:.1?}", r.fastest),
            r.bytes
        );
    }
}

/// A screen full of text with a few styles per row, like a busy app.
fn scene(shift: usize) -> Buffer {
    let area = Rect::new(0, 0, WIDTH, HEIGHT);
    let mut buf = Buffer::new(area);
    let plain = Style::new();
    let cyan = Style::new().fg(Color::Cyan).bold();
    let dim = Style::new().dim();
    for y in 0..HEIGHT {
        let text: String = (0..WIDTH as usize)
            .map(|x| char::from(b'a' + ((x + y as usize + shift) % 26) as u8))
            .collect();
        buf.set_string(0, y, &text[..60], plain);
        buf.set_string(60, y, &text[60..90], cyan);
        buf.set_string(90, y, &text[90..], dim);
    }
    buf
}

fn scene_with_cell(mut buf: Buffer) -> Buffer {
    buf.set_string(100, 30, "X", Style::new().fg(Color::Red));
    buf
}

fn scene_with_line(mut buf: Buffer) -> Buffer {
    buf.set_string(0, 30, &"#".repeat(WIDTH as usize), Style::new().bold());
    buf
}

/// Draws `a` and `b` on alternate frames.
fn alternate(a: Buffer, b: Buffer) -> impl FnMut() -> usize {
    let mut renderer = Renderer::new(WIDTH, HEIGHT);
    let mut flip = false;
    move || {
        flip = !flip;
        let scene = if flip { &a } else { &b };
        renderer.draw(|f| f.buffer_mut().clone_from(scene)).len()
    }
}

fn diffs() -> Vec<Result> {
    let base = scene(0);
    let mut cases = vec![
        // What it costs to put the frame in the buffer, without the diff.
        case("(baseline) copy the scene into a buffer", {
            let a = base.clone();
            let mut target = Buffer::new(a.area());
            move || {
                target.clone_from(&a);
                black_box(&target);
                0
            }
        }),
        // A frame that draws nothing: clearing the buffer and comparing it.
        case("(baseline) an empty frame", {
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || renderer.draw(|_| {}).len()
        }),
        case("identical", alternate(base.clone(), base.clone())),
        case(
            "one cell differs",
            alternate(base.clone(), scene_with_cell(base.clone())),
        ),
        case(
            "one line differs",
            alternate(base.clone(), scene_with_line(base.clone())),
        ),
        case("every cell differs", alternate(base.clone(), scene(1))),
    ];
    cases.push(case("first frame (repaint everything)", {
        let a = base.clone();
        let mut renderer = Renderer::new(WIDTH, HEIGHT);
        move || {
            renderer.invalidate();
            renderer.draw(|f| f.buffer_mut().clone_from(&a)).len()
        }
    }));
    cases.into_iter().map(measure).collect()
}

/// The pieces of a typical screen: a header, a list beside a transcript, an
/// input, a progress bar and a status line.
fn layout_frame(
    renderer: &mut Renderer,
    items: &[String],
    text: &str,
    selected: usize,
    tick: usize,
) -> usize {
    let mut list_state = ListState::new();
    list_state.select(Some(selected));
    let input = InputState::with_text("explain the renderer to me");
    renderer
        .draw(|frame| {
            let [header, body, input_area, status] = Layout::column()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Fill(1),
                    Constraint::Fixed(3),
                    Constraint::Fixed(1),
                ])
                .split_array(frame.area());
            let [side, main] = Layout::row()
                .constraints([Constraint::Percent(25), Constraint::Fill(1)])
                .split_array(body);
            frame.render_widget(Paragraph::new("crewtui  agent  files"), header);
            frame.render_stateful_widget(
                List::new(items.iter().map(String::as_str))
                    .block(Block::bordered().title("files"))
                    .highlight_style(Style::new().reverse()),
                side,
                &list_state,
            );
            frame.render_widget(
                Paragraph::new(text)
                    .block(Block::bordered().title("chat"))
                    .wrap(Wrap::Word),
                main,
            );
            frame.render_input(Input::new().block(Block::bordered()), input_area, &input);
            let [bar, right] = Layout::row()
                .constraints([Constraint::Fill(1), Constraint::Fixed(10)])
                .split_array(status);
            frame.render_widget(Progress::new((tick % 100) as f64 / 100.0), bar);
            frame.render_widget(Paragraph::new(format!("{tick}")), right);
        })
        .len()
}

fn layouts() -> Vec<Result> {
    let items: Vec<String> = (0..80).map(|i| format!("file_{i}.rs")).collect();
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(40);
    let cases = vec![
        case("layout: selection moves", {
            let (items, text) = (items.clone(), text.clone());
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            let mut i = 0;
            move || {
                i += 1;
                layout_frame(&mut renderer, &items, &text, i % 40, 0)
            }
        }),
        case("layout: nothing changes", {
            let (items, text) = (items.clone(), text.clone());
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || layout_frame(&mut renderer, &items, &text, 3, 0)
        }),
        case("layout: repaint everything", {
            let (items, text) = (items.clone(), text.clone());
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || {
                renderer.invalidate();
                layout_frame(&mut renderer, &items, &text, 3, 0)
            }
        }),
    ];
    cases.into_iter().map(measure).collect()
}

/// Entries of a line or two, and one in five long enough to wrap several
/// times at this width.
fn transcript(entries: usize) -> HistoryState {
    let mut state = HistoryState::new();
    for i in 0..entries {
        let filler = if i % 5 == 0 { 40 } else { i % 7 };
        state.push(format!(
            "entry {i}: a line of the transcript with enough words in it to wrap now and then, {}",
            "lorem ipsum ".repeat(filler)
        ));
    }
    state
}

fn history_frame(renderer: &mut Renderer, state: &HistoryState, area: Rect) -> usize {
    renderer
        .draw(|frame| {
            frame.render_stateful_widget(History::new(), area, state);
        })
        .len()
}

fn histories() -> Vec<Result> {
    let area = Rect::new(0, 0, WIDTH - 1, HEIGHT);
    let bar = Rect::new(WIDTH - 1, 0, 1, HEIGHT);
    let cases = vec![
        case("history 20,000: steady frame", {
            let state = transcript(20_000);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || history_frame(&mut renderer, &state, area)
        }),
        case("history 200: steady frame", {
            let state = transcript(200);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || history_frame(&mut renderer, &state, area)
        }),
        case("history 20,000: streamed token, short lines", {
            let mut state = transcript(20_000);
            state.push("agent:");
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            let mut n = 0;
            move || {
                n += 1;
                state.append(if n % 8 == 0 { "token\n" } else { "token " });
                history_frame(&mut renderer, &state, area)
            }
        }),
        case("history 20,000: streamed token, one growing line", {
            let mut state = transcript(20_000);
            state.push("agent:");
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || {
                state.append("token ");
                history_frame(&mut renderer, &state, area)
            }
        }),
        case("history 20,000: scroll by one row", {
            let mut state = transcript(20_000);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            history_frame(&mut renderer, &state, area);
            state.scroll_up(5_000);
            let mut down = false;
            move || {
                down = !down;
                if down {
                    state.scroll_down(1);
                } else {
                    state.scroll_up(1);
                }
                history_frame(&mut renderer, &state, area)
            }
        }),
        case("history 20,000: width changes each frame", {
            let state = transcript(20_000);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            let mut narrow = false;
            move || {
                narrow = !narrow;
                let w = if narrow { WIDTH - 20 } else { WIDTH };
                renderer.resize(w, HEIGHT);
                history_frame(&mut renderer, &state, Rect::new(0, 0, w - 1, HEIGHT))
            }
        }),
        case("history 20,000: steady frame with a scrollbar", {
            let state = transcript(20_000);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            move || {
                renderer
                    .draw(|frame| {
                        frame.render_stateful_widget(History::new(), area, &state);
                        frame.render_widget(
                            Scrollbar::vertical()
                                .content(state.content_rows())
                                .viewport(state.viewport_rows())
                                .position(state.position()),
                            bar,
                        );
                    })
                    .len()
            }
        }),
        case("history 20,000: width change, then the scrollbar", {
            let state = transcript(20_000);
            let mut renderer = Renderer::new(WIDTH, HEIGHT);
            let mut narrow = false;
            move || {
                narrow = !narrow;
                let w = if narrow { WIDTH - 20 } else { WIDTH };
                renderer.resize(w, HEIGHT);
                let area = Rect::new(0, 0, w - 1, HEIGHT);
                let bar = Rect::new(w - 1, 0, 1, HEIGHT);
                renderer
                    .draw(|frame| {
                        frame.render_stateful_widget(History::new(), area, &state);
                        frame.render_widget(
                            Scrollbar::vertical()
                                .content(state.content_rows())
                                .viewport(state.viewport_rows())
                                .position(state.position()),
                            bar,
                        );
                    })
                    .len()
            }
        }),
    ];
    cases.into_iter().map(measure).collect()
}

fn main() {
    // `cargo test --benches` runs this target without `--bench`; there is
    // nothing to check in that case.
    if !std::env::args().any(|a| a == "--bench") {
        return;
    }
    println!("crewtui frame benchmarks, {WIDTH}x{HEIGHT} cells");
    print("diffing two buffers", &diffs());
    print("a typical layout", &layouts());
    print("a long transcript", &histories());
}

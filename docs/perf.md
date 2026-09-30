# Performance

Numbers from `cargo bench`, which runs `benches/frame.rs`. It has no harness and no dependencies: each case is warmed up, then sampled 25 times, and the table shows the median and the fastest sample per frame, plus the bytes a frame wrote to the terminal on average.

These were taken on a Ryzen 5 7600 under WSL2 with rustc 1.96, on a 200x60 screen (12,000 cells). Treat them as a comparison between rows, not as promises. The median and the fastest sample differ by 20 to 30% in some cases on this machine, so a gap smaller than that is noise. Every gap used below to make a decision is larger.

## Diffing two buffers

Each case draws two scenes on alternate frames, so the renderer always has something to compare. The two baselines are what a frame costs without a diff worth the name: copying a prepared scene into the buffer, and drawing nothing at all.

| Case | Cell by cell | Rows skipped first | Bytes |
|---|---|---|---|
| (baseline) copy the scene | 102 µs | 104 µs | 0 |
| (baseline) an empty frame | 96 µs | 26 µs | 0 |
| identical | 202 µs | 128 µs | 0 |
| one cell differs | 202 µs | 131 µs | 20 |
| one line differs | 200 µs | 138 µs | 221 |
| every cell differs | 274 µs | 273 µs | 13,551 |
| first frame, repaint everything | 278 µs | 283 µs | 13,565 |

"Cell by cell" walks every cell and compares it with the one in the previous buffer. "Rows skipped first" compares each row as a slice before looking at its cells, and moves on when the two rows are equal. The two columns come from separate runs of the same bench, before and after the change.

Skipping rows wins wherever most of the screen stays put, which is nearly every frame of a real app: an unchanged frame goes from 202 to 128 µs, and clearing and comparing an empty screen from 96 to 26 µs. When every cell differs the extra comparison stops at the first cell of each row and the difference is inside the noise. So the renderer skips rows. Dirty rectangles and tracking which cells a widget touched were not tried. An unchanged frame costs 128 µs against 104 µs for copying the scene into the buffer, and part of that difference is clearing the buffer, which the empty-frame case puts at 26 µs. That leaves the diff itself small next to drawing.

The bytes column is the same in both. It is what makes a frame cheap for the terminal: one changed cell writes 20 bytes, one line 221, and a full repaint 13.5 KB.

## A typical layout

A header, a bordered list of 80 files beside a wrapped paragraph, an input, a progress bar and a status line, drawn with `Layout` on every frame. The list items and the text are built once, outside the timed loop.

| Case | Cell by cell | Rows skipped first | Bytes |
|---|---|---|---|
| the selection moves | 440 µs | 386 µs | 127 |
| nothing changes | 445 µs | 365 µs | 0 |
| repaint everything | 496 µs | 516 µs | 7,325 |

Most of the time is the widgets drawing into the buffer, not the diff. At 400 µs a frame the app has 40 times more room than a 60 fps cap asks for.

## A long transcript

`History` at 200x60, wrapped by word, with 20,000 entries. Most are a line or two, and one in five is long enough to wrap into four or five rows.

| Case | Time | Bytes |
|---|---|---|
| steady frame | 535 µs | 0 |
| the same with 200 entries | 562 µs | 0 |
| steady frame, with a scrollbar drawn | 559 µs | 0 |
| a streamed token, a new line every 8 tokens | 404 µs | 478 |
| a streamed token, all on one growing line | 608 µs | 241 |
| scroll by one row, 5,000 rows up | 678 µs | 7,714 |
| the width changes on every frame | 792 µs | 11,592 |
| the width changes, and the scrollbar wants the total | 75 ms | 12,049 |

The steady frame costs the same with 200 entries as with 20,000, which is the point of keeping the row count of every line of every entry: a frame wraps what is on screen and nothing else. The frame tests in `src/widgets/history.rs` check the same thing without a clock, by counting how many entries and lines a frame looks at, so a regression fails a test instead of showing up as a number nobody reads.

A streamed token writes a few hundred bytes on average instead of redrawing the transcript. The two streaming rows are not the same case. With a newline every few tokens the last line stays short. With everything on one line, the line being streamed is measured again on each token, so the frame gets slower as the line grows: on a scratch copy of this bench, 4,000 tokens into one line a frame took about 1.1 ms instead of 0.6. Streamed messages usually have newlines, but a paragraph of 30 lines that arrives as one line does not.

A width change is the expensive case. The frame itself is under a millisecond, because only what is on screen has to be counted again. The last row is different. A scrollbar needs the total number of rows, and after a width change that means counting every entry again: 75 ms for 20,000 entries here. It is paid once per width, and cached after that, but an app that drags its window edge with a scrollbar on a very long transcript will feel it. Making that cheaper is tracked in #54.

## The reference app under stress

`a_long_transcript_with_streaming_never_repaints_the_screen_per_token` in `src/agent_tests.rs` runs the agent from `examples/agent.rs` headless. It starts with 20,000 lines of history and streams six answers of 400 tokens into it. A token step sends the token, a newline every 9 tokens, a spinner tick every third token, a tool call every 40 tokens (ten per answer), a background clock tick every 50 tokens (the app turns every fifth tick into a note), and a typed character every 25 tokens. A frame is drawn after every token step, which is more often than the runtime draws. After each answer the screen is resized to 90x30 and back to 120x40.

The frame sizes don't depend on timing, so the test asserts on them with limits about a quarter above what the code writes today:

- no token frame writes half of a full repaint or more;
- the mean over all token frames stays under 420 bytes, and the median under 32;
- both resizes repaint, and a frame drawn twice with nothing in between writes nothing the second time.

On a 120x40 screen a full repaint is 6,455 bytes. Over the 2,400 token frames (`cargo test --release --lib a_long_transcript -- --nocapture` prints these):

| | Bytes | Time |
|---|---|---|
| median | 23 | 232 µs |
| mean | 334 | |
| 99th percentile | 2,780 | 479 µs |
| max | 2,993 | |

The times are from one run and move by tens of percent between runs, so the test only prints them. Most tokens change a few cells of the last line, hence the median of 23 bytes. A token that starts a new line scrolls the history pane and every row of it changes; that is the 2.8 KB tail. A terminal scroll region would make it cheaper, and it was not tried.

To see where a frame's time goes, the run was split into stages with a timer around each, in a scratch test that is not kept. Updating the state costs about 0.1 µs per token, drawing the view 190 µs, and diffing plus building the bytes 20 µs. Nearly all of the view is `History`, which wraps and draws the rows on screen.

The slow frame is the resize. A width change with a scrollbar has to measure every entry again to know the total number of rows, and each of the twelve resize frames in the run took 28 to 33 ms. It is the case tracked in #54. It doesn't happen while streaming, only when the window size changes.

## Running it

```
cargo bench
```

`cargo test` does not build or run it. `cargo test --benches` builds it and runs it with no arguments, where it exits at once.

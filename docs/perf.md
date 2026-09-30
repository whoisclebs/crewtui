# Performance

Numbers from `cargo bench`, which runs `benches/frame.rs`. It has no harness and no dependencies: each case is warmed up, then sampled 25 times, and the table shows the median and the fastest sample per frame, plus the bytes the frame wrote to the terminal.

These were taken on a Ryzen 5 7600 under WSL2 with rustc 1.96, on a 200x60 screen (12,000 cells). Treat them as a comparison between rows, not as promises. Run-to-run noise is around 5%.

## Diffing two buffers

Each case draws two scenes on alternate frames, so the renderer always has something to compare. The two baselines are what a frame costs without a diff worth the name: copying a prepared scene into the buffer, and drawing nothing at all.

| Case | Cell by cell | Rows skipped first | Bytes |
|---|---|---|---|
| (baseline) copy the scene | 102 µs | 104 µs | 0 |
| (baseline) an empty frame | 96 µs | 26 µs | 0 |
| identical | 202 µs | 132 µs | 0 |
| one cell differs | 202 µs | 143 µs | 20 |
| one line differs | 200 µs | 136 µs | 221 |
| every cell differs | 274 µs | 280 µs | 13,551 |
| first frame, repaint everything | 278 µs | 279 µs | 13,565 |

"Cell by cell" walks every cell and compares it with the one in the previous buffer. "Rows skipped first" compares each row as a slice before looking at its cells, and moves on when the two rows are equal.

Skipping rows wins wherever most of the screen stays put, which is nearly every frame of a real app: an unchanged frame goes from 202 to 132 µs, and clearing and comparing an empty screen from 96 to 26 µs. When every cell differs the extra comparison stops at the first cell of each row and the difference is inside the noise. So the renderer skips rows. Dirty rectangles and tracking which cells a widget touched were not tried: with the row check the diff of an unchanged frame adds about 28 µs to the 104 µs it takes to put the scene in the buffer, and the rest of a real frame is drawing.

The bytes column is the same in both. It is what makes a frame cheap for the terminal: one changed cell writes 20 bytes, one line 221, and a full repaint 13.5 KB.

## A typical layout

A header, a bordered list of 80 files beside a wrapped paragraph, an input, a progress bar and a status line, drawn with `Layout` on every frame.

| Case | Cell by cell | Rows skipped first | Bytes |
|---|---|---|---|
| the selection moves | 440 µs | 396 µs | 127 |
| nothing changes | 445 µs | 382 µs | 0 |
| repaint everything | 496 µs | 484 µs | 7,325 |

Most of the time is the widgets drawing into the buffer, not the diff. At 400 µs a frame the app has 40 times more room than a 60 fps cap asks for.

## A long transcript

`History` at 200x60, wrapped by word, with 20,000 entries of about one to three rows each.

| Case | Time | Bytes |
|---|---|---|
| steady frame | 464 µs | 0 |
| a streamed token appended to the last entry | 529 µs | 158 |
| scroll by one row, 5,000 rows up | 583 µs | 2,285 |
| the width changes on every frame | 736 µs | 10,247 |
| the same steady frame with 200 entries | 456 µs | 0 |
| steady frame plus the numbers for a scrollbar | 469 µs | 0 |

The steady frame costs the same with 200 entries as with 20,000, which is the point of keeping the row count of each entry: a frame wraps what is on screen and nothing else. A streamed token costs about 65 µs more than a steady frame, and writes 158 bytes instead of redrawing the transcript. A width change is the expensive case, and it is still under a millisecond. Counting the rows of the whole transcript, which a scrollbar wants, is paid when the width changes and is cached after that.

The frame tests in `src/widgets/history.rs` check the same thing without a clock, by counting how many entries and lines a frame looks at. That way a regression fails a test instead of showing up as a number nobody reads.

## Running it

```
cargo bench
```

`cargo test` also builds the bench and runs it with no arguments, where it exits at once.

# Dependencies

Direct dependencies of the core crate. The target is three or fewer.
A dependency is added in the same change that first uses it.

| Crate | Added in | Why |
|---|---|---|
| `libc` | #6 | termios raw mode, signal handling and pty helpers. Calling these through `std` isn't possible, and hand-written FFI declarations would be less portable than the crate. |
| `unicode-width` | #3 | Display width tables. Wrong widths corrupt the screen, and the tables change with each Unicode release. |
| `unicode-segmentation` | #3 | Grapheme cluster boundaries for cell writes, wrapping and cursor movement. |

`crewtui-rich`, the crate in `crates/rich`, depends on `crewtui` and on nothing else.

No async runtime. No `crossterm`, no `termion`: input parsing and terminal
control are part of what this crate is for.

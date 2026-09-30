# 2. Draw into a buffer, diff against the previous one

Accepted.

## Context

The app's view has to become bytes on a terminal, many times a second, while an answer streams into a transcript. Rewriting the screen for every token is what this must not do: it flickers, it costs bandwidth over ssh, and it is slow.

## Options

1. Widgets write escape sequences directly.
2. The view builds a tree, and the renderer reconciles it with the last tree.
3. Widgets draw into a grid of cells. The renderer keeps the grid it drew last, compares the two, and writes the difference.

## Decision

Option 3. `view` draws into a `Frame`, which wraps a `Buffer` of `Cell`s. The `Renderer` holds two buffers, diffs `current` against `previous`, returns the bytes and swaps them.

The diff compares rows as slices first and only looks at cells in rows that differ. That was measured against comparing every cell: an unchanged frame goes from 202 to 128 µs and an empty one from 96 to 26 µs on a 200x60 screen, and a frame where everything changes costs the same. Dirty rectangles and per-widget change tracking were not tried; the numbers are in `docs/perf.md`.

`previous` is a belief about the terminal, not a fact, so there is one recovery path, `Renderer::invalidate`. The next frame clears the screen and paints everything. The runtime calls it after a failed or short write, a resize, and SIGCONT, and apps can ask for it with `Cmd::repaint`.

## Consequences

Drawing is a pure function of state into a buffer, so tests draw a view, feed the bytes to a terminal model and compare. No widget knows what is on the terminal.

Every frame costs a full draw of the view, even when one cell changed. On a 120x40 screen that is around 200 µs for the reference app. An app whose view is expensive has to make it cheaper (`History` caches what it measured) since there is no partial redraw.

A change that moves many cells, like a scroll, rewrites all of them. A terminal scroll region could avoid that and is not used.

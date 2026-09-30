# 4. A cell holds a grapheme cluster, and widths come from `unicode-width`

Accepted.

## Context

`text.len()` is bytes and `text.chars().count()` is not columns. Emoji, CJK and combining marks break both. A wrong width doesn't crash, it shifts the rest of the row and leaves stale cells that a diff never fixes.

## Options

1. Treat a `char` as a cell.
2. Treat a grapheme cluster as a cell, with its width from a Unicode table.
3. Ask the terminal how wide something is.

## Decision

Option 2, with `unicode-segmentation` for the clusters and `unicode-width` for the widths. A cell stores one cluster (inline up to 14 bytes, on the heap beyond that, for ZWJ sequences) and its style. A wide cluster takes a cell and a continuation cell after it. Writing over half of a wide glyph clears the other half.

Widths are 0, 1 or 2. Control characters, tabs included, have width 0 and are not drawn. Bidi control characters are dropped from cells. Truncation and wrapping cut on cluster boundaries, and a glyph wider than the space available is left out of a truncation and gets its own line in a wrap, so wrapping always makes progress. The cursor in `Input` moves by cluster.

Both crates are direct dependencies (`docs/dependencies.md`). The tables change with each Unicode release and are not something to keep by hand.

## Consequences

Terminals disagree with the tables on some emoji sequences. When they do, rows containing them can drift by a cell until the next repaint. Option 3 would fix that and needs a round trip and terminal support, so it isn't done.

Right-to-left text is not shaped or reordered. It is stored and drawn in logical order.

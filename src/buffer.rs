//! Cells and the buffer widgets draw into.

use crate::text::grapheme_width;
use crate::{Rect, Style};
use unicode_segmentation::UnicodeSegmentation;

const INLINE: usize = 14;

/// Storage for one grapheme cluster. Clusters up to 14 bytes (nearly all
/// of them) live inline; longer ones, such as ZWJ emoji families, spill to
/// the heap.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Symbol {
    Inline { len: u8, bytes: [u8; INLINE] },
    Heap(Box<Box<str>>),
}

impl Symbol {
    fn new(s: &str) -> Self {
        if s.len() <= INLINE {
            let mut bytes = [0; INLINE];
            bytes[..s.len()].copy_from_slice(s.as_bytes());
            Symbol::Inline {
                len: s.len() as u8,
                bytes,
            }
        } else {
            Symbol::Heap(Box::new(s.into()))
        }
    }

    fn as_str(&self) -> &str {
        match self {
            // The bytes were copied from a `&str` of exactly this length.
            Symbol::Inline { len, bytes } => {
                std::str::from_utf8(&bytes[..*len as usize]).unwrap_or("")
            }
            Symbol::Heap(s) => s,
        }
    }
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// One terminal cell: a grapheme cluster and its style.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Cell {
    symbol: Symbol,
    style: Style,
}

impl Cell {
    /// A blank cell with no style.
    pub fn blank() -> Self {
        Cell {
            symbol: Symbol::new(" "),
            style: Style::new(),
        }
    }

    /// The text shown in this cell.
    pub fn symbol(&self) -> &str {
        self.symbol.as_str()
    }

    /// The cell's style.
    pub fn style(&self) -> Style {
        self.style
    }

    /// True for the trailing half of a wide glyph. Such a cell has no text
    /// of its own; the glyph to its left covers it.
    pub fn is_continuation(&self) -> bool {
        self.symbol().is_empty()
    }

    fn continuation(style: Style) -> Self {
        Cell {
            symbol: Symbol::new(""),
            style,
        }
    }

    /// Replaces the text, keeping the style. Doesn't maintain the wide-glyph
    /// invariant, so it stays internal; outside code goes through
    /// [`Buffer::set_string`].
    pub(crate) fn set_symbol(&mut self, symbol: &str) -> &mut Self {
        self.symbol = Symbol::new(symbol);
        self
    }

    /// Layers `style` on top of the current one.
    pub fn patch_style(&mut self, style: Style) -> &mut Self {
        self.style = self.style.patch(style);
        self
    }

    /// Replaces the style outright.
    pub fn set_style(&mut self, style: Style) -> &mut Self {
        self.style = style;
        self
    }

    /// Back to a blank, unstyled cell.
    pub fn reset(&mut self) {
        *self = Cell::blank();
    }
}

impl Default for Cell {
    fn default() -> Self {
        Cell::blank()
    }
}

impl std::fmt::Debug for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cell")
            .field("symbol", &self.symbol())
            .field("style", &self.style)
            .finish()
    }
}

/// A grid of cells covering a [`Rect`].
///
/// Coordinates passed to the accessors are absolute (they include the
/// buffer's origin). Anything outside the area is ignored on write and
/// `None` on read.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Buffer {
    area: Rect,
    cells: Vec<Cell>,
}

impl Buffer {
    /// A buffer of blank cells.
    pub fn new(area: Rect) -> Self {
        Buffer {
            area,
            cells: vec![Cell::blank(); area.area()],
        }
    }

    /// The area this buffer covers.
    pub fn area(&self) -> Rect {
        self.area
    }

    /// All cells in row-major order.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    fn index_of(&self, x: u16, y: u16) -> Option<usize> {
        if !self.area.contains(x, y) {
            return None;
        }
        let row = (y - self.area.y) as usize;
        let col = (x - self.area.x) as usize;
        Some(row * self.area.width as usize + col)
    }

    /// The cell at `(x, y)`.
    pub fn get(&self, x: u16, y: u16) -> Option<&Cell> {
        self.index_of(x, y).map(|i| &self.cells[i])
    }

    /// Mutable access to the cell at `(x, y)`.
    pub fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
        self.index_of(x, y).map(|i| &mut self.cells[i])
    }

    /// Writes `text` starting at `(x, y)` and returns the column after the
    /// last cell written, so calls can be chained.
    ///
    /// Text is placed by grapheme cluster and display width. A wide glyph
    /// fills its cell plus a continuation cell; overwriting either half of
    /// a wide glyph blanks the other half. A wide glyph that would cross
    /// the right edge (or the left edge of the buffer) is replaced by a
    /// blank instead of being half drawn. Zero-width clusters attach to the
    /// previous cell; control characters are skipped. Writes outside the
    /// buffer are clipped.
    pub fn set_string(&mut self, x: u16, y: u16, text: &str, style: Style) -> u16 {
        if y < self.area.y || y >= self.area.bottom() {
            return x;
        }
        let left = self.area.x as usize;
        let right = self.area.right() as usize;
        let mut col = x as usize;
        let mut wrote_any = false;
        for g in text.graphemes(true) {
            let w = grapheme_width(g);
            if w == 0 {
                if wrote_any && g.chars().next().is_some_and(|c| !c.is_control()) {
                    self.append_to_previous(col, y, g);
                }
                continue;
            }
            if col + w > right {
                if col < right {
                    self.put(col as u16, y, " ", 1, style);
                    col = right;
                }
                break;
            }
            if col < left {
                if col + w > left {
                    self.put(left as u16, y, " ", 1, style);
                }
                col += w;
                continue;
            }
            self.put(col as u16, y, g, w, style);
            wrote_any = true;
            col += w;
        }
        col.min(u16::MAX as usize) as u16
    }

    /// Joins a zero-width cluster onto the last drawn cell. It is dropped
    /// when that would change how many columns the cell takes, since the
    /// cell layout (and the terminal's cursor) would then disagree, and
    /// when it is a bidirectional control, which could reorder text
    /// outside the buffer.
    fn append_to_previous(&mut self, col: usize, y: u16, g: &str) {
        if g.chars().any(is_bidi_control) {
            return;
        }
        let mut x = col;
        while x > self.area.x as usize {
            x -= 1;
            let Some(cell) = self.get_mut(x as u16, y) else {
                return;
            };
            if cell.is_continuation() {
                continue;
            }
            let joined = format!("{}{}", cell.symbol(), g);
            if grapheme_width(&joined) == grapheme_width(cell.symbol()) {
                cell.set_symbol(&joined);
            }
            return;
        }
    }

    /// Writes one glyph of width `w` at `(x, y)`, which must be inside the
    /// buffer with room for `w` columns, and repairs any wide glyph it
    /// overlaps.
    fn put(&mut self, x: u16, y: u16, symbol: &str, w: usize, style: Style) {
        for k in 0..w {
            self.free_cell(x + k as u16, y);
        }
        if let Some(cell) = self.get_mut(x, y) {
            cell.set_symbol(symbol).set_style(style);
        }
        if w == 2 {
            if let Some(cell) = self.get_mut(x + 1, y) {
                *cell = Cell::continuation(style);
            }
        }
    }

    /// Blanks the other half of any wide glyph that covers `(x, y)`.
    fn free_cell(&mut self, x: u16, y: u16) {
        let Some(cell) = self.get(x, y) else { return };
        if cell.is_continuation() {
            if x > self.area.x {
                if let Some(head) = self.get_mut(x - 1, y) {
                    head.set_symbol(" ");
                }
            }
        } else if self.get(x + 1, y).is_some_and(Cell::is_continuation) {
            if let Some(tail) = self.get_mut(x + 1, y) {
                tail.set_symbol(" ");
            }
        }
    }

    /// Layers `style` on every cell of `area`, clipped to the buffer.
    pub fn set_style(&mut self, area: Rect, style: Style) {
        let area = self.area.intersection(area);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(cell) = self.get_mut(x, y) {
                    cell.patch_style(style);
                }
            }
        }
    }

    /// Resets every cell to blank.
    pub fn reset(&mut self) {
        self.cells.iter_mut().for_each(Cell::reset);
    }

    /// Changes the covered area. The content is cleared, because keeping a
    /// stale layout after a resize is a source of artifacts.
    pub fn resize(&mut self, area: Rect) {
        self.area = area;
        self.cells.clear();
        self.cells.resize(area.area(), Cell::blank());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;
    use crate::text::grapheme_width;

    #[test]
    fn new_buffer_is_blank() {
        let b = Buffer::new(Rect::new(0, 0, 3, 2));
        assert_eq!(b.cells().len(), 6);
        assert!(
            b.cells()
                .iter()
                .all(|c| c.symbol() == " " && c.style() == Style::new())
        );
    }

    #[test]
    fn set_and_get_with_origin_offset() {
        let mut b = Buffer::new(Rect::new(5, 10, 3, 2));
        let s = Style::new().fg(Color::Red);
        b.set_string(6, 11, "x", s);
        let c = b.get(6, 11).unwrap();
        assert_eq!(c.symbol(), "x");
        assert_eq!(c.style(), s);
        assert!(b.get(4, 10).is_none());
    }

    #[test]
    fn out_of_bounds_write_is_ignored() {
        let mut b = Buffer::new(Rect::new(0, 0, 2, 2));
        let before = b.clone();
        b.set_string(2, 0, "x", Style::new());
        b.set_string(0, 2, "x", Style::new());
        b.set_style(Rect::new(10, 10, 5, 5), Style::new().bold());
        assert_eq!(b, before);
    }

    #[test]
    fn set_style_is_clipped_and_layered() {
        let mut b = Buffer::new(Rect::new(0, 0, 3, 3));
        b.set_string(1, 1, "a", Style::new().fg(Color::Red));
        b.set_style(Rect::new(1, 1, 10, 10), Style::new().bold());
        let c = b.get(1, 1).unwrap();
        assert_eq!(c.style(), Style::new().fg(Color::Red).bold());
        assert_eq!(b.get(0, 0).unwrap().style(), Style::new());
        assert_eq!(b.get(2, 2).unwrap().style(), Style::new().bold());
    }

    #[test]
    fn long_clusters_survive_round_trip() {
        let family = "👨‍👩‍👧‍👦";
        assert!(family.len() > INLINE);
        let mut b = Buffer::new(Rect::new(0, 0, 2, 1));
        b.set_string(0, 0, family, Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), family);
        b.set_string(0, 0, "é", Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), "é");
    }

    #[test]
    fn reset_and_resize_clear_content() {
        let mut b = Buffer::new(Rect::new(0, 0, 2, 2));
        b.set_string(0, 0, "x", Style::new().bold());
        b.reset();
        assert_eq!(b, Buffer::new(Rect::new(0, 0, 2, 2)));
        b.set_string(1, 1, "y", Style::new());
        b.resize(Rect::new(0, 0, 4, 1));
        assert_eq!(b, Buffer::new(Rect::new(0, 0, 4, 1)));
    }

    #[test]
    fn empty_area_is_fine() {
        let mut b = Buffer::new(Rect::new(0, 0, 0, 5));
        b.set_string(0, 0, "x", Style::new());
        assert!(b.cells().is_empty());
        assert!(b.get(0, 0).is_none());
    }

    fn row_text(b: &Buffer, y: u16) -> String {
        (b.area().x..b.area().right())
            .map(|x| b.get(x, y).unwrap().symbol())
            .collect()
    }

    /// Every continuation cell follows a width-2 head, and every width-2
    /// head is followed by a continuation cell.
    fn assert_wide_invariant(b: &Buffer) {
        let a = b.area();
        for y in a.y..a.bottom() {
            for x in a.x..a.right() {
                let c = b.get(x, y).unwrap();
                if c.is_continuation() {
                    assert!(x > a.x, "continuation in first column at ({x},{y})");
                    let head = b.get(x - 1, y).unwrap();
                    assert_eq!(
                        grapheme_width(head.symbol()),
                        2,
                        "orphan continuation at ({x},{y})"
                    );
                } else if grapheme_width(c.symbol()) == 2 {
                    let tail = b.get(x + 1, y);
                    assert!(
                        tail.is_some_and(Cell::is_continuation),
                        "headless wide glyph at ({x},{y})"
                    );
                }
            }
        }
    }

    #[test]
    fn wide_glyph_takes_two_cells() {
        let mut b = Buffer::new(Rect::new(0, 0, 6, 1));
        let end = b.set_string(0, 0, "a中b", Style::new());
        assert_eq!(end, 4);
        assert_eq!(b.get(1, 0).unwrap().symbol(), "中");
        assert!(b.get(2, 0).unwrap().is_continuation());
        assert_eq!(b.get(3, 0).unwrap().symbol(), "b");
        assert_wide_invariant(&b);
    }

    #[test]
    fn wide_glyph_at_right_edge_is_blanked_not_split() {
        let mut b = Buffer::new(Rect::new(0, 0, 3, 1));
        let end = b.set_string(0, 0, "ab中c", Style::new());
        assert_eq!(end, 3);
        assert_eq!(row_text(&b, 0), "ab ");
        assert_wide_invariant(&b);
    }

    #[test]
    fn overwriting_head_or_tail_blanks_the_other_half() {
        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "中中", Style::new());
        b.set_string(1, 0, "x", Style::new());
        assert_eq!(row_text(&b, 0), " x中");
        assert_wide_invariant(&b);

        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "中中", Style::new());
        b.set_string(0, 0, "x", Style::new());
        assert_eq!(row_text(&b, 0), "x 中");
        assert_wide_invariant(&b);
    }

    #[test]
    fn wide_glyph_over_two_wide_glyphs_repairs_both() {
        let mut b = Buffer::new(Rect::new(0, 0, 6, 1));
        b.set_string(0, 0, "中中中", Style::new());
        b.set_string(1, 0, "字", Style::new());
        assert_eq!(row_text(&b, 0), " 字 中");
        assert_wide_invariant(&b);
    }

    #[test]
    fn zero_width_marks_attach_to_previous_cell() {
        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "e\u{301}x", Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), "e\u{301}");
        assert_eq!(b.get(1, 0).unwrap().symbol(), "x");
        // A leading mark with nothing to attach to is dropped.
        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "\u{301}a", Style::new());
        assert_eq!(row_text(&b, 0), "a   ");
    }

    #[test]
    fn a_standalone_mark_after_a_control_char_attaches_when_width_is_unchanged() {
        // The tab splits the cluster, so the mark arrives on its own.
        let mut b = Buffer::new(Rect::new(0, 0, 5, 1));
        let end = b.set_string(0, 0, "中\t\u{301}x", Style::new());
        assert_eq!(end, 3);
        assert_eq!(b.get(0, 0).unwrap().symbol(), "中\u{301}");
        assert!(b.get(1, 0).unwrap().is_continuation());
        assert_eq!(b.get(2, 0).unwrap().symbol(), "x");
        assert_wide_invariant(&b);

        let mut b = Buffer::new(Rect::new(0, 0, 3, 1));
        b.set_string(0, 0, "a\t\u{301}", Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), "a\u{301}");
    }

    #[test]
    fn a_mark_that_would_widen_the_cell_is_dropped() {
        // U+2764 alone is one column; with U+FE0F it becomes two.
        let mut b = Buffer::new(Rect::new(0, 0, 5, 1));
        let end = b.set_string(0, 0, "\u{2764}\t\u{fe0f}x", Style::new());
        assert_eq!(end, 2);
        assert_eq!(b.get(0, 0).unwrap().symbol(), "\u{2764}");
        assert_eq!(b.get(1, 0).unwrap().symbol(), "x");
        assert_wide_invariant(&b);
    }

    #[test]
    fn bidi_controls_are_not_written() {
        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        b.set_string(0, 0, "a\u{202e}b", Style::new());
        assert_eq!(row_text(&b, 0), "ab  ");
        b.set_string(0, 0, "a\t\u{202e}b", Style::new());
        assert_eq!(row_text(&b, 0), "ab  ");
    }

    #[test]
    fn control_characters_are_skipped() {
        let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
        let end = b.set_string(0, 0, "a\tb\u{1b}c", Style::new());
        assert_eq!(end, 3);
        assert_eq!(row_text(&b, 0), "abc ");
    }

    #[test]
    fn emoji_sequences_are_single_wide_cells() {
        let mut b = Buffer::new(Rect::new(0, 0, 6, 1));
        b.set_string(0, 0, "👨‍👩‍👧‍👦🇧🇷", Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), "👨‍👩‍👧‍👦");
        assert_eq!(b.get(2, 0).unwrap().symbol(), "🇧🇷");
        assert_wide_invariant(&b);
    }

    #[test]
    fn writing_with_offset_origin_and_left_clip() {
        let mut b = Buffer::new(Rect::new(2, 1, 4, 2));
        // Starts one column left of the buffer: the wide glyph straddles the edge.
        let end = b.set_string(1, 1, "中ab", Style::new());
        assert_eq!(end, 5);
        assert_eq!(row_text(&b, 1), " ab ");
        assert_eq!(b.set_string(2, 0, "zzz", Style::new()), 2);
        assert_eq!(b.set_string(2, 5, "zzz", Style::new()), 2);
        assert_wide_invariant(&b);
    }

    #[test]
    fn random_overwrites_never_leave_stale_halves() {
        let pieces = [
            "a",
            "é",
            "e\u{301}",
            "中",
            "字",
            "😀",
            "👨‍👩‍👧‍👦",
            "🇧🇷",
            " ",
            "\u{301}",
            "\t",
            "ab",
            "中文",
        ];
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut b = Buffer::new(Rect::new(3, 0, 11, 3));
        for _ in 0..5000 {
            let x = (next() % 18) as u16;
            let y = (next() % 3) as u16;
            let n = next() % 4 + 1;
            let text: String = (0..n)
                .map(|_| pieces[(next() % pieces.len() as u64) as usize])
                .collect();
            b.set_string(x, y, &text, Style::new());
            assert_wide_invariant(&b);
        }
    }

    #[test]
    fn cell_stays_small() {
        assert!(
            std::mem::size_of::<Cell>() <= 32,
            "{}",
            std::mem::size_of::<Cell>()
        );
    }
}

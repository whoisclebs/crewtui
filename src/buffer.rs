//! Cells and the buffer widgets draw into.

use std::sync::Arc;

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

/// Whether `url` can go into an OSC 8 sequence as it is.
fn valid_link(url: &str) -> bool {
    !url.is_empty() && url.len() <= 2048 && url.bytes().all(|b| (0x21..=0x7e).contains(&b))
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
    pub(crate) fn patch_style(&mut self, style: Style) -> &mut Self {
        self.style = self.style.patch(style);
        self
    }

    /// Replaces the style outright.
    pub(crate) fn set_style(&mut self, style: Style) -> &mut Self {
        self.style = style;
        self
    }

    /// Back to a blank, unstyled cell.
    pub(crate) fn reset(&mut self) {
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
    /// Hyperlinks, as runs of cells on one row. Kept sorted by row and then
    /// column, without overlaps, and with touching runs of the same URL
    /// merged, so two buffers that link the same cells are equal. Most
    /// frames have none, which keeps the common path free of them.
    links: Vec<LinkRun>,
}

/// Cells `x0..x1` of row `y` that belong to the hyperlink `url`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct LinkRun {
    y: u16,
    x0: u16,
    x1: u16,
    url: Arc<str>,
}

impl LinkRun {
    pub(crate) fn covers(&self, x: u16) -> bool {
        self.x0 <= x && x < self.x1
    }

    pub(crate) fn end(&self) -> u16 {
        self.x1
    }

    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

impl Buffer {
    /// A buffer of blank cells.
    pub fn new(area: Rect) -> Self {
        Buffer {
            area,
            cells: vec![Cell::blank(); area.area()],
            links: Vec::new(),
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

    /// Mutable access to the cell at `(x, y)`. Internal: writes must keep
    /// wide glyphs intact, so they go through the `Buffer` methods.
    pub(crate) fn get_mut(&mut self, x: u16, y: u16) -> Option<&mut Cell> {
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
        if !self.links.is_empty() {
            self.unlink(y, x, x + w as u16);
        }
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

    /// Makes every cell of `area`, clipped to the buffer, part of the
    /// hyperlink `url`, or of none for `None`. Text drawn over these cells
    /// afterwards replaces the link, like it replaces the style. Both
    /// halves of a wide glyph get it.
    ///
    /// The link is written as an OSC 8 sequence by the renderer, and is not
    /// part of the text, so it doesn't count toward any width. A URL that is
    /// empty, longer than 2,048 bytes, or has anything but printable ASCII
    /// in it, spaces and control characters included, is ignored: an escape
    /// sequence in it would end the OSC and let the text after it run as
    /// terminal commands. Percent-encode what does not fit.
    pub fn set_link(&mut self, area: Rect, url: Option<&str>) {
        let link: Option<Arc<str>> = match url {
            Some(url) if valid_link(url) => Some(Arc::from(url)),
            Some(_) => return,
            None => None,
        };
        let area = self.area.intersection(area);
        if area.is_empty() || (link.is_none() && self.links.is_empty()) {
            return;
        }
        for y in area.y..area.bottom() {
            // A terminal can't link the two halves of a wide glyph
            // differently, so touching either half links the whole glyph.
            let mut x0 = area.x;
            let mut x1 = area.right();
            if self.get(x0, y).is_some_and(Cell::is_continuation) && x0 > self.area.x {
                x0 -= 1;
            }
            if self.get(x1 - 1, y).is_some_and(|c| !c.is_continuation())
                && self.get(x1, y).is_some_and(Cell::is_continuation)
            {
                x1 += 1;
            }
            self.unlink(y, x0, x1);
            if let Some(url) = &link {
                self.add_run(LinkRun {
                    y,
                    x0,
                    x1,
                    url: url.clone(),
                });
            }
        }
    }

    /// The URL of the hyperlink the cell at `(x, y)` belongs to, if any.
    pub fn link_at(&self, x: u16, y: u16) -> Option<&str> {
        let row = self.row_links(y);
        row.iter().find(|r| r.covers(x)).map(LinkRun::url)
    }

    /// The link runs of row `y`, left to right.
    pub(crate) fn row_links(&self, y: u16) -> &[LinkRun] {
        if self.links.is_empty() {
            return &[];
        }
        let start = self.links.partition_point(|r| r.y < y);
        let end = start + self.links[start..].partition_point(|r| r.y == y);
        &self.links[start..end]
    }

    /// Takes cells `x0..x1` of row `y` out of any link, cutting the runs
    /// that stick out of it.
    fn unlink(&mut self, y: u16, x0: u16, x1: u16) {
        let start = self.links.partition_point(|r| r.y < y);
        let end = start + self.links[start..].partition_point(|r| r.y == y);
        let mut kept = Vec::new();
        for run in self.links.drain(start..end) {
            if run.x1 <= x0 || run.x0 >= x1 {
                kept.push(run);
                continue;
            }
            if run.x0 < x0 {
                kept.push(LinkRun {
                    x1: x0,
                    ..run.clone()
                });
            }
            if run.x1 > x1 {
                kept.push(LinkRun { x0: x1, ..run });
            }
        }
        self.links.splice(start..start, kept);
    }

    /// Adds a run over cells that are in no link, keeping the runs sorted
    /// and merging it with touching runs of the same URL.
    fn add_run(&mut self, mut run: LinkRun) {
        let at = self
            .links
            .partition_point(|r| (r.y, r.x0) < (run.y, run.x0));
        if at > 0 {
            let before = &self.links[at - 1];
            if before.y == run.y && before.x1 == run.x0 && before.url == run.url {
                run.x0 = before.x0;
                self.links.remove(at - 1);
                return self.add_run(run);
            }
        }
        if let Some(after) = self.links.get(at) {
            if after.y == run.y && after.x0 == run.x1 && after.url == run.url {
                run.x1 = after.x1;
                self.links.remove(at);
            }
        }
        let at = self
            .links
            .partition_point(|r| (r.y, r.x0) < (run.y, run.x0));
        self.links.insert(at, run);
    }

    /// Layers `style` on every cell of `area`, clipped to the buffer. A
    /// terminal can't style the two halves of a wide glyph differently, so
    /// touching either half styles the whole glyph.
    pub fn set_style(&mut self, area: Rect, style: Style) {
        let area = self.area.intersection(area);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let other = match self.get(x, y) {
                    Some(c) if c.is_continuation() => x.checked_sub(1),
                    Some(_) if self.get(x + 1, y).is_some_and(Cell::is_continuation) => Some(x + 1),
                    _ => None,
                };
                for x in std::iter::once(x).chain(other) {
                    if let Some(cell) = self.get_mut(x, y) {
                        cell.patch_style(style);
                    }
                }
            }
        }
    }

    /// Resets every cell of `area`, clipped to the buffer, to a blank cell
    /// with no style and no hyperlink, whatever was drawn there. It is what
    /// to do before drawing a popup over other content.
    ///
    /// A wide glyph that the area cuts through, at its left or right edge, is
    /// blanked whole, so no half of it is left behind.
    pub fn clear(&mut self, area: Rect) {
        let area = self.area.intersection(area);
        if area.is_empty() {
            return;
        }
        for y in area.y..area.bottom() {
            // The glyph on the left edge that started outside the area.
            if area.x > self.area.x && self.get(area.x, y).is_some_and(Cell::is_continuation) {
                self.blank_cell(area.x - 1, y);
            }
            // The glyph on the right edge whose second half is outside.
            let last = area.right() - 1;
            if self.get(last + 1, y).is_some_and(Cell::is_continuation) {
                self.blank_cell(last + 1, y);
            }
            if !self.links.is_empty() {
                self.unlink(y, area.x, area.right());
            }
            for x in area.x..area.right() {
                self.blank_cell(x, y);
            }
        }
    }

    fn blank_cell(&mut self, x: u16, y: u16) {
        if let Some(cell) = self.get_mut(x, y) {
            cell.reset();
        }
    }

    /// Resets every cell to blank.
    pub(crate) fn reset(&mut self) {
        self.cells.iter_mut().for_each(Cell::reset);
        self.links.clear();
    }

    /// Changes the covered area. The content is cleared, because keeping a
    /// stale layout after a resize is a source of artifacts.
    pub(crate) fn resize(&mut self, area: Rect) {
        self.area = area;
        self.cells.clear();
        self.cells.resize(area.area(), Cell::blank());
        self.links.clear();
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
    fn set_style_on_half_a_wide_glyph_styles_both_halves() {
        for x in [0, 1] {
            let mut b = Buffer::new(Rect::new(0, 0, 4, 1));
            b.set_string(0, 0, "中a", Style::new());
            b.set_style(Rect::new(x, 0, 1, 1), Style::new().bold());
            assert_eq!(b.get(0, 0).unwrap().style(), Style::new().bold());
            assert_eq!(b.get(1, 0).unwrap().style(), Style::new().bold());
            assert_eq!(b.get(2, 0).unwrap().style(), Style::new());
        }
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
    fn set_link_marks_cells_and_text_written_over_them_replaces_the_link() {
        let mut b = Buffer::new(Rect::new(0, 0, 8, 2));
        b.set_string(0, 0, "abcdef", Style::new());
        b.set_link(Rect::new(1, 0, 3, 1), Some("https://e.com"));
        assert_eq!(b.link_at(0, 0), None);
        assert_eq!(b.link_at(1, 0), Some("https://e.com"));
        assert_eq!(b.link_at(3, 0), Some("https://e.com"));
        assert_eq!(b.link_at(4, 0), None);
        // The text is what it was.
        let row: String = (0..6).map(|x| b.get(x, 0).unwrap().symbol()).collect();
        assert_eq!(row, "abcdef");
        b.set_string(2, 0, "XY", Style::new());
        assert_eq!(b.link_at(1, 0), Some("https://e.com"));
        assert_eq!(b.link_at(2, 0), None);
        assert_eq!(b.link_at(3, 0), None);
        b.set_link(Rect::new(0, 0, 8, 2), None);
        assert!(b.links.is_empty());
    }

    #[test]
    fn a_link_covers_both_halves_of_a_wide_glyph_even_when_the_area_touches_one() {
        let mut b = Buffer::new(Rect::new(0, 0, 6, 1));
        b.set_string(0, 0, "a中b", Style::new());
        b.set_link(Rect::new(2, 0, 1, 1), Some("https://e.com"));
        assert_eq!(b.link_at(1, 0), Some("https://e.com"));
        assert_eq!(b.link_at(2, 0), Some("https://e.com"));
        assert_eq!(b.link_at(3, 0), None);
    }

    #[test]
    fn links_with_anything_but_printable_ascii_are_ignored_and_the_area_is_clipped() {
        let mut b = Buffer::new(Rect::new(0, 0, 3, 1));
        for url in ["", "a b", "a\x1bb", "a\x07b", "\u{7f}", "é", "a\nb"] {
            b.set_link(Rect::new(0, 0, 3, 1), Some(url));
            assert!(b.links.is_empty(), "{url:?}");
        }
        b.set_link(Rect::new(0, 0, 3, 1), Some(&"a".repeat(2049)));
        assert!(b.links.is_empty());
        b.set_link(Rect::new(2, 0, 50, 50), Some("https://e.com"));
        assert_eq!(b.link_at(2, 0), Some("https://e.com"));
        assert_eq!(b.link_at(1, 0), None);
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

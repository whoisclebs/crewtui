//! Cells and the buffer widgets draw into.

use crate::{Rect, Style};

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

    /// Replaces the text, keeping the style.
    pub fn set_symbol(&mut self, symbol: &str) -> &mut Self {
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

    /// Writes one grapheme cluster into one cell. Out-of-bounds writes are
    /// ignored. This does not look at display width; use it for symbols you
    /// know occupy exactly one column.
    pub fn set_symbol(&mut self, x: u16, y: u16, symbol: &str, style: Style) {
        if let Some(cell) = self.get_mut(x, y) {
            cell.set_symbol(symbol).set_style(style);
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
        b.set_symbol(6, 11, "x", s);
        let c = b.get(6, 11).unwrap();
        assert_eq!(c.symbol(), "x");
        assert_eq!(c.style(), s);
        assert!(b.get(4, 10).is_none());
    }

    #[test]
    fn out_of_bounds_write_is_ignored() {
        let mut b = Buffer::new(Rect::new(0, 0, 2, 2));
        let before = b.clone();
        b.set_symbol(2, 0, "x", Style::new());
        b.set_symbol(0, 2, "x", Style::new());
        b.set_style(Rect::new(10, 10, 5, 5), Style::new().bold());
        assert_eq!(b, before);
    }

    #[test]
    fn set_style_is_clipped_and_layered() {
        let mut b = Buffer::new(Rect::new(0, 0, 3, 3));
        b.set_symbol(1, 1, "a", Style::new().fg(Color::Red));
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
        b.set_symbol(0, 0, family, Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), family);
        b.set_symbol(0, 0, "é", Style::new());
        assert_eq!(b.get(0, 0).unwrap().symbol(), "é");
    }

    #[test]
    fn reset_and_resize_clear_content() {
        let mut b = Buffer::new(Rect::new(0, 0, 2, 2));
        b.set_symbol(0, 0, "x", Style::new().bold());
        b.reset();
        assert_eq!(b, Buffer::new(Rect::new(0, 0, 2, 2)));
        b.set_symbol(1, 1, "y", Style::new());
        b.resize(Rect::new(0, 0, 4, 1));
        assert_eq!(b, Buffer::new(Rect::new(0, 0, 4, 1)));
    }

    #[test]
    fn empty_area_is_fine() {
        let mut b = Buffer::new(Rect::new(0, 0, 0, 5));
        b.set_symbol(0, 0, "x", Style::new());
        assert!(b.cells().is_empty());
        assert!(b.get(0, 0).is_none());
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

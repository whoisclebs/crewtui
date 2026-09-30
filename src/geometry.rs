//! Rectangles in terminal cell coordinates.

/// A rectangle of terminal cells. The origin is the top-left corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    /// Column of the left edge.
    pub x: u16,
    /// Row of the top edge.
    pub y: u16,
    /// Width in columns.
    pub width: u16,
    /// Height in rows.
    pub height: u16,
}

impl Rect {
    /// Creates a rectangle. Width and height are clamped so the far edges
    /// stay inside `u16` range.
    pub const fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        let width = if x as u32 + width as u32 > u16::MAX as u32 {
            u16::MAX - x
        } else {
            width
        };
        let height = if y as u32 + height as u32 > u16::MAX as u32 {
            u16::MAX - y
        } else {
            height
        };
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// Number of cells covered.
    pub const fn area(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// True when the rectangle covers no cells.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Column just past the right edge.
    pub const fn right(&self) -> u16 {
        self.x.saturating_add(self.width)
    }

    /// Row just past the bottom edge.
    pub const fn bottom(&self) -> u16 {
        self.y.saturating_add(self.height)
    }

    /// True when the cell at `(x, y)` lies inside the rectangle.
    pub const fn contains(&self, x: u16, y: u16) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }

    /// The overlapping region of two rectangles, empty if they don't touch.
    pub fn intersection(&self, other: Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = self.right().min(other.right());
        let bottom = self.bottom().min(other.bottom());
        Rect {
            x,
            y,
            width: right.saturating_sub(x),
            height: bottom.saturating_sub(y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_clamps_to_u16_range() {
        let r = Rect::new(u16::MAX - 1, 0, 10, 3);
        assert_eq!(r.width, 1);
        assert_eq!(r.right(), u16::MAX);
    }

    #[test]
    fn contains_is_half_open() {
        let r = Rect::new(2, 3, 4, 2);
        assert!(r.contains(2, 3));
        assert!(r.contains(5, 4));
        assert!(!r.contains(6, 4));
        assert!(!r.contains(5, 5));
        assert!(!r.contains(1, 3));
    }

    #[test]
    fn intersection_of_disjoint_is_empty() {
        let a = Rect::new(0, 0, 3, 3);
        let b = Rect::new(5, 5, 3, 3);
        assert!(a.intersection(b).is_empty());
    }

    #[test]
    fn intersection_overlap() {
        let a = Rect::new(0, 0, 5, 5);
        let b = Rect::new(3, 2, 5, 5);
        assert_eq!(a.intersection(b), Rect::new(3, 2, 2, 3));
    }

    #[test]
    fn area_does_not_overflow() {
        assert_eq!(Rect::new(0, 0, u16::MAX, u16::MAX).area(), 65535 * 65535);
    }
}

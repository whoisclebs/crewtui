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

    /// A rectangle of `width` by `height` in the middle of this one, for a
    /// dialog or a popup. A size larger than this rectangle is clamped to it.
    /// When the space left over is odd, the extra cell goes to the right and
    /// the bottom.
    pub fn centered(&self, width: u16, height: u16) -> Rect {
        let width = width.min(self.width);
        let height = height.min(self.height);
        Rect {
            x: self.x + (self.width - width) / 2,
            y: self.y + (self.height - height) / 2,
            width,
            height,
        }
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
    fn centered_puts_the_extra_cell_right_and_below() {
        let r = Rect::new(2, 1, 11, 6);
        assert_eq!(r.centered(4, 3), Rect::new(5, 2, 4, 3));
        assert_eq!(r.centered(11, 6), r);
    }

    #[test]
    fn centered_is_clamped_to_the_parent() {
        let r = Rect::new(3, 4, 10, 5);
        assert_eq!(r.centered(50, 50), r);
        assert_eq!(r.centered(50, 1), Rect::new(3, 6, 10, 1));
        assert!(r.centered(0, 3).is_empty());
        assert!(Rect::new(0, 0, 0, 0).centered(4, 4).is_empty());
    }

    #[test]
    fn area_does_not_overflow() {
        assert_eq!(Rect::new(0, 0, u16::MAX, u16::MAX).area(), 65535 * 65535);
    }
}

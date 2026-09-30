//! Splitting a rectangle into rows or columns.
//!
//! A [`Layout`] is a function from a [`Rect`] and some constraints to a list
//! of rectangles. There is no tree and no solver: the rules below are
//! applied with integer arithmetic, in the same order every time, so the
//! same input always gives the same rectangles.

use crate::Rect;

/// How much of the main axis one item takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Constraint {
    /// Exactly this many cells.
    Fixed(u16),
    /// This percentage of the space left after the gaps, rounded down.
    Percent(u16),
    /// A share of whatever is left, in proportion to the weight.
    Fill(u16),
    /// At least this many cells, and a share of what is left beyond that.
    Min(u16),
    /// A share of what is left, but no more than this many cells.
    Max(u16),
}

/// What to do with space no item wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Justify {
    /// Items sit at the start; the space is left at the end.
    #[default]
    Start,
    /// Items are centered; an odd extra cell goes to the end.
    Center,
    /// Items sit at the end.
    End,
    /// The space is spread over the gaps between items. With a single item
    /// this is the same as `Start`.
    SpaceBetween,
}

/// Distances from the four edges of a rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges {
    /// Top.
    pub top: u16,
    /// Right.
    pub right: u16,
    /// Bottom.
    pub bottom: u16,
    /// Left.
    pub left: u16,
}

impl Edges {
    /// The same distance on every side.
    pub const fn all(n: u16) -> Self {
        Edges {
            top: n,
            right: n,
            bottom: n,
            left: n,
        }
    }

    /// `vertical` above and below, `horizontal` left and right.
    pub const fn symmetric(vertical: u16, horizontal: u16) -> Self {
        Edges {
            top: vertical,
            right: horizontal,
            bottom: vertical,
            left: horizontal,
        }
    }

    /// `area` with these distances taken off each side. The result is
    /// empty, and stays inside `area`, when they don't leave any room.
    pub fn shrink(self, area: Rect) -> Rect {
        // `Rect` has public fields, so the area may reach past `u16::MAX`.
        let area = Rect::new(area.x, area.y, area.width, area.height);
        let horizontal = u32::from(self.left) + u32::from(self.right);
        let vertical = u32::from(self.top) + u32::from(self.bottom);
        let width = u32::from(area.width).saturating_sub(horizontal) as u16;
        let height = u32::from(area.height).saturating_sub(vertical) as u16;
        Rect {
            x: area.x.saturating_add(self.left.min(area.width)),
            y: area.y.saturating_add(self.top.min(area.height)),
            width,
            height,
        }
    }
}

impl From<u16> for Edges {
    fn from(n: u16) -> Self {
        Edges::all(n)
    }
}

/// Splits an area into a row or a column of rectangles.
///
/// ```
/// use crewtui::{Constraint, Layout, Rect};
///
/// let area = Rect::new(0, 0, 80, 24);
/// let [header, body, footer] = Layout::column()
///     .constraints([Constraint::Fixed(1), Constraint::Fill(1), Constraint::Fixed(1)])
///     .split_array(area);
/// assert_eq!(header, Rect::new(0, 0, 80, 1));
/// assert_eq!(body, Rect::new(0, 1, 80, 22));
/// assert_eq!(footer, Rect::new(0, 23, 80, 1));
/// ```
///
/// The rules:
///
/// 1. The margin is taken off the area, then the gaps between items.
/// 2. `Fixed`, `Percent` and the minimum of `Min` are granted first. If they
///    add up to more than the room, the last items shrink first, down to
///    nothing, until they fit.
/// 3. What remains is shared by `Fill`, `Min` (weight 1) and `Max` (weight
///    1, up to its cap) in proportion to their weights. Whole cells only:
///    the cells that don't divide evenly go one each to the earliest items.
/// 4. Space nobody wanted is placed according to the [`Justify`].
/// 5. Each slot is shrunk by the padding.
///
/// Every rectangle lies inside the area, none overlap, and nothing panics
/// on an empty area or on constraints that can't be met.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    vertical: bool,
    constraints: Vec<Constraint>,
    gap: u16,
    margin: Edges,
    padding: Edges,
    alignment: Justify,
}

/// Sum of the sizes. A layout with tens of thousands of items can go past
/// `u32::MAX`, so this is a `u64`.
fn total(sizes: &[u32]) -> u64 {
    sizes.iter().map(|&s| u64::from(s)).sum()
}

impl Layout {
    /// Items side by side, dividing the width.
    pub fn row() -> Self {
        Layout::new(false)
    }

    /// Items one above the other, dividing the height.
    pub fn column() -> Self {
        Layout::new(true)
    }

    fn new(vertical: bool) -> Self {
        Layout {
            vertical,
            constraints: Vec::new(),
            gap: 0,
            margin: Edges::default(),
            padding: Edges::default(),
            alignment: Justify::Start,
        }
    }

    /// One constraint per item, in order.
    pub fn constraints(mut self, constraints: impl IntoIterator<Item = Constraint>) -> Self {
        self.constraints = constraints.into_iter().collect();
        self
    }

    /// Empty cells between neighbouring items.
    pub fn gap(mut self, gap: u16) -> Self {
        self.gap = gap;
        self
    }

    /// Space kept free around the whole layout.
    pub fn margin(mut self, margin: impl Into<Edges>) -> Self {
        self.margin = margin.into();
        self
    }

    /// Space kept free inside each item's slot.
    pub fn padding(mut self, padding: impl Into<Edges>) -> Self {
        self.padding = padding.into();
        self
    }

    /// Where items go when they don't use up all the room.
    pub fn justify(mut self, justify: Justify) -> Self {
        self.alignment = justify;
        self
    }

    /// The rectangles for `area`, one per constraint.
    pub fn split(&self, area: Rect) -> Vec<Rect> {
        let n = self.constraints.len();
        if n == 0 {
            return Vec::new();
        }
        let area = Rect::new(area.x, area.y, area.width, area.height);
        let inner = self.margin.shrink(area);
        let length = u32::from(if self.vertical {
            inner.height
        } else {
            inner.width
        });
        let gaps = (u64::from(self.gap) * (n as u64 - 1)).min(u64::from(u32::MAX)) as u32;
        let available = length.saturating_sub(gaps);

        let sizes = self.sizes(available);
        let used = total(&sizes) as u32;
        let spare = available.saturating_sub(used);

        let (start, extra_gap, extra_first) = self.place(spare, n);
        let mut pos = start;
        let mut rects = Vec::with_capacity(n);
        for (i, size) in sizes.iter().enumerate() {
            let main_len = *size;
            let slot = if self.vertical {
                Rect {
                    x: inner.x,
                    y: inner
                        .y
                        .saturating_add(pos.min(u32::from(inner.height)) as u16),
                    width: inner.width,
                    height: main_len.min(u32::from(inner.height)) as u16,
                }
            } else {
                Rect {
                    x: inner
                        .x
                        .saturating_add(pos.min(u32::from(inner.width)) as u16),
                    y: inner.y,
                    width: main_len.min(u32::from(inner.width)) as u16,
                    height: inner.height,
                }
            };
            rects.push(self.padding.shrink(slot));
            let between = u32::from(self.gap) + extra_gap + u32::from((i as u32) < extra_first);
            pos = pos.saturating_add(main_len).saturating_add(between);
        }
        rects
    }

    /// Like [`Layout::split`], for when the number of items is known at
    /// compile time.
    ///
    /// # Panics
    ///
    /// Panics if the layout doesn't have exactly `N` constraints, which is
    /// a mistake in the code that built it.
    pub fn split_array<const N: usize>(&self, area: Rect) -> [Rect; N] {
        let rects = self.split(area);
        let len = rects.len();
        rects
            .try_into()
            .unwrap_or_else(|_| panic!("expected {N} constraints, the layout has {len}"))
    }

    /// The length of each item along the main axis, before alignment.
    fn sizes(&self, available: u32) -> Vec<u32> {
        let n = self.constraints.len();
        let mut sizes: Vec<u32> = self
            .constraints
            .iter()
            .map(|c| match *c {
                Constraint::Fixed(v) | Constraint::Min(v) => u32::from(v),
                Constraint::Percent(p) => available * u32::from(p) / 100,
                Constraint::Fill(_) | Constraint::Max(_) => 0,
            })
            .collect();

        // Too much asked for: the last items give way first.
        let mut over = total(&sizes).saturating_sub(u64::from(available));
        for size in sizes.iter_mut().rev() {
            if over == 0 {
                break;
            }
            let cut = over.min(u64::from(*size)) as u32;
            *size -= cut;
            over -= u64::from(cut);
        }

        // Share what is left among the flexible items, in whole cells.
        let mut remaining = available - total(&sizes) as u32;
        let mut open: Vec<usize> = (0..n)
            .filter(|&i| {
                !matches!(
                    self.constraints[i],
                    Constraint::Fixed(_) | Constraint::Percent(_)
                ) && self.weight(i) > 0
            })
            .collect();
        while remaining > 0 && !open.is_empty() {
            let total: u32 = open.iter().map(|&i| self.weight(i)).sum();
            let pool = remaining;
            let mut granted = vec![0u32; open.len()];
            for (k, &i) in open.iter().enumerate() {
                granted[k] = pool * self.weight(i) / total;
            }
            // Cells that didn't divide evenly go to the earliest items.
            let mut leftover = pool - granted.iter().sum::<u32>();
            for g in granted.iter_mut() {
                if leftover == 0 {
                    break;
                }
                *g += 1;
                leftover -= 1;
            }
            let mut given = 0;
            let mut still_open = Vec::new();
            for (k, &i) in open.iter().enumerate() {
                let room = match self.constraints[i] {
                    Constraint::Max(cap) => u32::from(cap).saturating_sub(sizes[i]),
                    _ => u32::MAX,
                };
                let take = granted[k].min(room);
                sizes[i] += take;
                given += take;
                let full = matches!(self.constraints[i], Constraint::Max(cap) if sizes[i] >= u32::from(cap));
                if !full {
                    still_open.push(i);
                }
            }
            remaining -= given;
            // Nobody hit a cap, so everything was handed out. Otherwise the
            // capped items drop out and their unused share goes round again.
            if given == 0 || still_open.len() == open.len() {
                break;
            }
            open = still_open;
        }
        sizes
    }

    fn weight(&self, i: usize) -> u32 {
        match self.constraints[i] {
            Constraint::Fill(w) => u32::from(w),
            Constraint::Min(_) => 1,
            Constraint::Max(cap) => u32::from(cap > 0),
            Constraint::Fixed(_) | Constraint::Percent(_) => 0,
        }
    }

    /// Where the first item starts, the extra space added to every gap and
    /// the number of gaps that get one more cell.
    fn place(&self, spare: u32, n: usize) -> (u32, u32, u32) {
        match self.alignment {
            Justify::Start => (0, 0, 0),
            Justify::Center => (spare / 2, 0, 0),
            Justify::End => (spare, 0, 0),
            Justify::SpaceBetween if n > 1 => {
                let gaps = n as u32 - 1;
                (0, spare / gaps, spare % gaps)
            }
            Justify::SpaceBetween => (0, 0, 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Constraint::*;

    fn cols(constraints: impl IntoIterator<Item = Constraint>, width: u16) -> Vec<(u16, u16)> {
        Layout::row()
            .constraints(constraints)
            .split(Rect::new(0, 0, width, 3))
            .iter()
            .map(|r| (r.x, r.width))
            .collect()
    }

    #[test]
    fn fixed_items_take_exactly_what_they_ask() {
        assert_eq!(cols([Fixed(10), Fixed(20)], 80), vec![(0, 10), (10, 20)]);
    }

    #[test]
    fn percent_rounds_down_of_the_space_after_gaps() {
        assert_eq!(cols([Percent(50), Percent(50)], 11), vec![(0, 5), (5, 5)]);
        let r = Layout::row()
            .constraints([Percent(50), Percent(50)])
            .gap(2)
            .split(Rect::new(0, 0, 12, 1));
        assert_eq!((r[0].x, r[0].width, r[1].x, r[1].width), (0, 5, 7, 5));
    }

    #[test]
    fn fill_shares_what_is_left_by_weight() {
        assert_eq!(
            cols([Fixed(10), Fill(1), Fill(3)], 50),
            vec![(0, 10), (10, 10), (20, 30)]
        );
    }

    #[test]
    fn cells_that_do_not_divide_evenly_go_to_the_earliest_items() {
        assert_eq!(
            cols([Fill(1), Fill(1), Fill(1)], 10),
            vec![(0, 4), (4, 3), (7, 3)]
        );
        assert_eq!(
            cols([Fill(1), Fill(1), Fill(1)], 11),
            vec![(0, 4), (4, 4), (8, 3)]
        );
    }

    #[test]
    fn min_keeps_its_minimum_and_grows_into_what_is_left() {
        assert_eq!(cols([Min(10), Fixed(20)], 50), vec![(0, 30), (30, 20)]);
        // Not enough room for both: the later item gives way first.
        assert_eq!(cols([Min(10), Fixed(45)], 50), vec![(0, 10), (10, 40)]);
    }

    #[test]
    fn max_takes_a_share_but_never_more_than_its_cap() {
        assert_eq!(cols([Max(5), Fill(1)], 40), vec![(0, 5), (5, 35)]);
        assert_eq!(cols([Max(30), Fill(1)], 40), vec![(0, 20), (20, 20)]);
    }

    #[test]
    fn what_a_capped_item_cannot_use_goes_to_the_others() {
        assert_eq!(
            cols([Max(2), Max(3), Fill(1)], 30),
            vec![(0, 2), (2, 3), (5, 25)]
        );
    }

    #[test]
    fn only_capped_items_leave_room_that_alignment_places() {
        let place = |a| {
            Layout::row()
                .constraints([Max(4), Max(4)])
                .justify(a)
                .split(Rect::new(0, 0, 20, 1))
                .iter()
                .map(|r| r.x)
                .collect::<Vec<_>>()
        };
        assert_eq!(place(Justify::Start), vec![0, 4]);
        assert_eq!(place(Justify::Center), vec![6, 10]);
        assert_eq!(place(Justify::End), vec![12, 16]);
        assert_eq!(place(Justify::SpaceBetween), vec![0, 16]);
    }

    #[test]
    fn space_between_spreads_the_rest_over_the_gaps() {
        let r = Layout::row()
            .constraints([Fixed(2), Fixed(2), Fixed(2)])
            .justify(Justify::SpaceBetween)
            .split(Rect::new(0, 0, 13, 1));
        // 7 spare cells over 2 gaps: 4 then 3.
        assert_eq!(r.iter().map(|r| r.x).collect::<Vec<_>>(), vec![0, 6, 11]);
        let one = Layout::row()
            .constraints([Fixed(2)])
            .justify(Justify::SpaceBetween)
            .split(Rect::new(0, 0, 13, 1));
        assert_eq!(one[0].x, 0);
    }

    #[test]
    fn overconstrained_layouts_shrink_the_last_items_first() {
        assert_eq!(
            cols([Fixed(30), Fixed(30), Fixed(30)], 50),
            vec![(0, 30), (30, 20), (50, 0)]
        );
        assert_eq!(cols([Percent(80), Percent(80)], 10), vec![(0, 8), (8, 2)]);
        assert_eq!(
            cols([Min(30), Fill(1), Fixed(10)], 20),
            vec![(0, 20), (20, 0), (20, 0)]
        );
    }

    #[test]
    fn gaps_come_off_before_anything_is_shared() {
        assert_eq!(
            Layout::row()
                .constraints([Fill(1), Fill(1)])
                .gap(4)
                .split(Rect::new(0, 0, 20, 1))
                .iter()
                .map(|r| (r.x, r.width))
                .collect::<Vec<_>>(),
            vec![(0, 8), (12, 8)]
        );
    }

    #[test]
    fn a_gap_wider_than_the_area_leaves_empty_items_inside_it() {
        let r = Layout::row()
            .constraints([Fill(1), Fill(1), Fill(1)])
            .gap(10)
            .split(Rect::new(0, 0, 8, 1));
        assert!(r.iter().all(|r| r.width == 0 && r.x <= 8));
    }

    #[test]
    fn margin_shrinks_the_area_and_padding_shrinks_each_slot() {
        let r = Layout::row()
            .constraints([Fill(1), Fill(1)])
            .margin(Edges::symmetric(1, 2))
            .padding(1)
            .split(Rect::new(0, 0, 24, 10));
        // Area inside the margin: x 2..22, y 1..9. Slots 10 wide, then padded.
        assert_eq!(r[0], Rect::new(3, 2, 8, 6));
        assert_eq!(r[1], Rect::new(13, 2, 8, 6));
    }

    #[test]
    fn a_column_divides_the_height() {
        let r = Layout::column()
            .constraints([Fixed(1), Fill(1), Fixed(2)])
            .split(Rect::new(5, 7, 30, 20));
        assert_eq!(
            r,
            vec![
                Rect::new(5, 7, 30, 1),
                Rect::new(5, 8, 30, 17),
                Rect::new(5, 25, 30, 2)
            ]
        );
    }

    #[test]
    fn the_area_keeps_its_origin() {
        let r = Layout::row()
            .constraints([Fixed(3), Fixed(3)])
            .split(Rect::new(10, 4, 20, 2));
        assert_eq!(r, vec![Rect::new(10, 4, 3, 2), Rect::new(13, 4, 3, 2)]);
    }

    #[test]
    fn empty_areas_and_no_constraints_are_fine() {
        assert!(Layout::row().split(Rect::new(0, 0, 10, 10)).is_empty());
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(3, 3, 0, 5),
            Rect::new(3, 3, 5, 0),
        ] {
            let r = Layout::row()
                .constraints([Fixed(3), Fill(1), Percent(50)])
                .gap(1)
                .margin(2)
                .padding(1)
                .split(area);
            assert_eq!(r.len(), 3);
            assert!(r.iter().all(|r| r.is_empty()));
        }
    }

    #[test]
    fn extreme_values_do_not_overflow() {
        let area = Rect::new(u16::MAX - 10, u16::MAX - 10, 10, 10);
        let r = Layout::row()
            .constraints([
                Fixed(u16::MAX),
                Percent(u16::MAX),
                Fill(u16::MAX),
                Min(u16::MAX),
                Max(u16::MAX),
            ])
            .gap(u16::MAX)
            .margin(u16::MAX)
            .padding(u16::MAX)
            .split(area);
        assert_eq!(r.len(), 5);
        let big = Layout::row()
            .constraints([Fixed(u16::MAX), Fixed(u16::MAX)])
            .split(Rect::new(0, 0, u16::MAX, u16::MAX));
        assert_eq!(big[0].width as u32 + big[1].width as u32, u16::MAX as u32);
    }

    #[test]
    fn an_area_built_by_hand_past_the_end_of_u16_does_not_overflow() {
        // `Rect`'s fields are public, so nothing forces the clamp in `new`.
        let area = Rect {
            x: 65_000,
            y: 65_000,
            width: 1_000,
            height: 1_000,
        };
        let r = Layout::row()
            .constraints([Fixed(600), Fixed(600)])
            .split(area);
        // `right()` saturates, so check the real sum.
        assert!(
            r.iter()
                .all(|r| u32::from(r.x) + u32::from(r.width) <= u32::from(u16::MAX))
        );
        let m = Layout::column()
            .constraints([Fixed(600), Fixed(600)])
            .margin(Edges {
                left: 600,
                top: 600,
                ..Edges::default()
            })
            .split(area);
        assert_eq!(m.len(), 2);
        let shrunk = Edges::all(700).shrink(area);
        assert!(u32::from(shrunk.x) + u32::from(shrunk.width) <= u32::from(u16::MAX));
    }

    #[test]
    fn a_huge_number_of_items_does_not_overflow_the_sums() {
        let items = vec![Fixed(u16::MAX); 70_000];
        let r = Layout::row()
            .constraints(items)
            .gap(u16::MAX)
            .split(Rect::new(0, 0, 100, 1));
        assert_eq!(r.len(), 70_000);
        assert!(r.iter().all(|r| r.right() <= 100));
    }

    #[test]
    fn split_array_gives_a_fixed_size_array() {
        let [a, b] = Layout::row()
            .constraints([Fixed(4), Fill(1)])
            .split_array(Rect::new(0, 0, 10, 1));
        assert_eq!((a.width, b.width), (4, 6));
    }

    #[test]
    #[should_panic(expected = "expected 3 constraints, the layout has 2")]
    fn split_array_with_the_wrong_count_says_so() {
        let _: [Rect; 3] = Layout::row()
            .constraints([Fixed(1), Fixed(1)])
            .split_array(Rect::new(0, 0, 10, 1));
    }

    #[test]
    fn edges_shrink_clamps_to_the_area() {
        let a = Rect::new(2, 3, 4, 4);
        assert_eq!(Edges::all(1).shrink(a), Rect::new(3, 4, 2, 2));
        let gone = Edges::all(9).shrink(a);
        assert!(gone.is_empty());
        assert!(a.intersection(gone).area() == 0 || gone.x >= a.x && gone.x <= a.right());
    }

    /// Random constraints, gaps, margins, paddings and alignments: every
    /// rectangle stays inside the area, no two overlap and the count matches.
    #[test]
    fn random_layouts_stay_inside_the_area_and_never_overlap() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % m) as u16
        };
        for _ in 0..20_000 {
            let n = next(7) as usize;
            let constraints: Vec<Constraint> = (0..n)
                .map(|_| match next(5) {
                    0 => Fixed(next(60)),
                    1 => Percent(next(130)),
                    2 => Fill(next(5)),
                    3 => Min(next(60)),
                    _ => Max(next(60)),
                })
                .collect();
            let alignment = [
                Justify::Start,
                Justify::Center,
                Justify::End,
                Justify::SpaceBetween,
            ][next(4) as usize];
            let layout = Layout::row()
                .constraints(constraints.clone())
                .gap(next(6))
                .margin(Edges::symmetric(next(4), next(4)))
                .padding(Edges::symmetric(next(3), next(3)))
                .justify(alignment);
            let layout = if next(2) == 0 {
                layout
            } else {
                Layout {
                    vertical: true,
                    ..layout
                }
            };
            let area = Rect::new(next(200), next(200), next(120), next(60));
            let rects = layout.split(area);
            assert_eq!(rects.len(), n, "{constraints:?}");
            for r in &rects {
                if r.is_empty() {
                    continue;
                }
                assert!(
                    r.x >= area.x
                        && r.y >= area.y
                        && r.right() <= area.right()
                        && r.bottom() <= area.bottom(),
                    "{r:?} outside {area:?} for {layout:?}"
                );
            }
            for (i, a) in rects.iter().enumerate() {
                for b in &rects[i + 1..] {
                    assert!(
                        a.is_empty() || b.is_empty() || a.intersection(*b).is_empty(),
                        "{a:?} overlaps {b:?} for {layout:?} in {area:?}"
                    );
                }
            }
            assert_eq!(layout.split(area), rects, "not deterministic");
        }
    }
}

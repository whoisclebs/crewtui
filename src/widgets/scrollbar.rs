use super::Widget;
use crate::{Buffer, Rect, Style};

/// Which way a [`Scrollbar`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// Down the right edge of its area.
    #[default]
    Vertical,
    /// Along the bottom edge of its area.
    Horizontal,
}

/// A track with a thumb that shows where a viewport sits in some content.
///
/// The three numbers are the app's own: how long the content is, how much of
/// it is visible, and where the visible part starts, all in the same unit
/// (lines, columns, items). The scrollbar keeps nothing between frames.
///
/// ```
/// use crewtui::widgets::{Scrollbar, Widget};
/// use crewtui::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 1, 10);
/// let mut buf = Buffer::new(area);
/// // 100 lines, 10 visible, scrolled to line 45.
/// Scrollbar::vertical().content(100).viewport(10).position(45).render(area, &mut buf);
/// ```
///
/// The thumb is proportional to the viewport and never smaller than one
/// cell, however long the content is. When everything fits, the thumb fills
/// the track. Give it a one-cell-wide area; it draws on the right edge
/// (vertical) or the bottom edge (horizontal) of whatever it gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scrollbar {
    orientation: Orientation,
    content: usize,
    viewport: usize,
    position: usize,
    track: &'static str,
    thumb: &'static str,
    track_style: Style,
    thumb_style: Style,
}

impl Scrollbar {
    /// A scrollbar down the right edge.
    pub fn vertical() -> Self {
        Scrollbar::new(Orientation::Vertical, "│")
    }

    /// A scrollbar along the bottom edge.
    pub fn horizontal() -> Self {
        Scrollbar::new(Orientation::Horizontal, "─")
    }

    fn new(orientation: Orientation, track: &'static str) -> Self {
        Scrollbar {
            orientation,
            content: 0,
            viewport: 0,
            position: 0,
            track,
            thumb: "█",
            track_style: Style::new(),
            thumb_style: Style::new(),
        }
    }

    /// How long the whole content is.
    pub fn content(mut self, length: usize) -> Self {
        self.content = length;
        self
    }

    /// How much of it is visible at once.
    pub fn viewport(mut self, length: usize) -> Self {
        self.viewport = length;
        self
    }

    /// Where the visible part starts. Past the last position it counts as
    /// the last.
    pub fn position(mut self, position: usize) -> Self {
        self.position = position;
        self
    }

    /// The symbols for the track and the thumb.
    pub fn symbols(mut self, track: &'static str, thumb: &'static str) -> Self {
        self.track = track;
        self.thumb = thumb;
        self
    }

    /// The style of the track and of the thumb.
    pub fn styles(mut self, track: Style, thumb: Style) -> Self {
        self.track_style = track;
        self.thumb_style = thumb;
        self
    }
}

/// Where the thumb starts and how long it is on a track of `track` cells.
fn thumb(track: u16, content: usize, viewport: usize, position: usize) -> (u16, u16) {
    let track = u128::from(track);
    if track == 0 {
        return (0, 0);
    }
    if content == 0 || viewport >= content {
        return (0, track as u16);
    }
    let (content, viewport) = (content as u128, viewport as u128);
    let len = ((track * viewport + content / 2) / content).clamp(1, track);
    let max = content - viewport;
    let position = (position as u128).min(max);
    let travel = track - len;
    let start = (travel * position + max / 2) / max;
    (start as u16, len as u16)
}

impl Widget for Scrollbar {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        let vertical = self.orientation == Orientation::Vertical;
        let track = if vertical { area.height } else { area.width };
        let (start, len) = thumb(track, self.content, self.viewport, self.position);
        for i in 0..track {
            let (symbol, style) = if i >= start && i < start + len {
                (self.thumb, self.thumb_style)
            } else {
                (self.track, self.track_style)
            };
            if vertical {
                buf.set_string(area.right() - 1, area.y + i, symbol, style);
            } else {
                buf.set_string(area.x + i, area.bottom() - 1, symbol, style);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    fn column(track: u16, content: usize, viewport: usize, position: usize) -> String {
        let area = Rect::new(0, 0, 1, track);
        let mut buf = Buffer::new(area);
        Scrollbar::vertical()
            .content(content)
            .viewport(viewport)
            .position(position)
            .render(area, &mut buf);
        (0..track)
            .map(|y| buf.get(0, y).unwrap().symbol())
            .collect()
    }

    #[test]
    fn the_thumb_is_proportional_to_the_viewport() {
        assert_eq!(column(10, 20, 10, 0), "█████│││││");
        assert_eq!(column(10, 20, 10, 10), "│││││█████");
        assert_eq!(column(10, 40, 10, 0), "███│││││││");
        assert_eq!(column(10, 100, 50, 25), "│││█████││");
    }

    #[test]
    fn the_thumb_is_never_smaller_than_one_cell() {
        assert_eq!(column(10, 1_000_000, 10, 0), "█│││││││││");
        assert_eq!(column(10, 1_000_000, 10, 999_990), "│││││││││█");
        assert_eq!(column(5, usize::MAX, 1, usize::MAX), "││││█");
        // First render, everything present: the case opentui#1526 got wrong.
        assert_eq!(column(8, 8, 8, 0), "████████");
    }

    #[test]
    fn a_position_past_the_end_counts_as_the_last() {
        assert_eq!(column(10, 20, 10, 500), column(10, 20, 10, 10));
    }

    #[test]
    fn when_everything_fits_the_thumb_fills_the_track() {
        assert_eq!(column(6, 5, 10, 0), "██████");
        assert_eq!(column(6, 10, 10, 3), "██████");
        assert_eq!(column(6, 0, 0, 0), "██████");
    }

    #[test]
    fn short_tracks_work() {
        assert_eq!(column(1, 100, 10, 50), "█");
        assert_eq!(column(2, 100, 10, 0), "█│");
        assert_eq!(column(2, 100, 10, 90), "│█");
        assert_eq!(column(0, 100, 10, 0), "");
    }

    #[test]
    fn a_horizontal_scrollbar_runs_along_the_bottom_edge() {
        let area = Rect::new(2, 1, 8, 3);
        let mut buf = Buffer::new(Rect::new(0, 0, 12, 5));
        Scrollbar::horizontal()
            .content(16)
            .viewport(8)
            .position(8)
            .render(area, &mut buf);
        let bottom: String = (2..10).map(|x| buf.get(x, 3).unwrap().symbol()).collect();
        assert_eq!(bottom, "────████");
        assert_eq!(buf.get(2, 1).unwrap().symbol(), " ");
    }

    #[test]
    fn a_vertical_scrollbar_draws_on_the_right_edge_of_a_wider_area() {
        let area = Rect::new(1, 0, 3, 4);
        let mut buf = Buffer::new(Rect::new(0, 0, 6, 4));
        Scrollbar::vertical()
            .content(8)
            .viewport(4)
            .render(area, &mut buf);
        assert_eq!(buf.get(3, 0).unwrap().symbol(), "█");
        assert_eq!(buf.get(3, 3).unwrap().symbol(), "│");
        assert_eq!(buf.get(1, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn symbols_and_styles_are_configurable() {
        let area = Rect::new(0, 0, 1, 4);
        let mut buf = Buffer::new(area);
        Scrollbar::vertical()
            .content(8)
            .viewport(4)
            .symbols("░", "▓")
            .styles(Style::new().fg(Color::Blue), Style::new().fg(Color::Red))
            .render(area, &mut buf);
        assert_eq!(buf.get(0, 0).unwrap().symbol(), "▓");
        assert_eq!(buf.get(0, 0).unwrap().style().fg, Some(Color::Red));
        assert_eq!(buf.get(0, 3).unwrap().symbol(), "░");
        assert_eq!(buf.get(0, 3).unwrap().style().fg, Some(Color::Blue));
    }

    #[test]
    fn empty_areas_and_areas_beyond_the_buffer_are_fine() {
        let mut buf = Buffer::new(Rect::new(0, 0, 3, 3));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 1, 0),
            Rect::new(9, 9, 2, 2),
        ] {
            Scrollbar::vertical()
                .content(10)
                .viewport(2)
                .render(area, &mut buf);
            Scrollbar::horizontal()
                .content(10)
                .viewport(2)
                .render(area, &mut buf);
        }
        assert!(buf.cells().iter().all(|c| c.symbol() == " "));
    }

    /// For every track, content, viewport and position: the thumb stays on
    /// the track, is at least one cell, and moves forward with the position.
    #[test]
    fn the_thumb_stays_on_the_track_and_only_moves_forward() {
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut next = move |m: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % m
        };
        for _ in 0..5_000 {
            let track = next(60) as u16 + 1;
            let content = next(100_000) as usize + 1;
            let viewport = next(content as u64 + 5) as usize;
            let mut last = 0;
            let mut positions: Vec<usize> = (0..10)
                .map(|_| next(content as u64 + 50) as usize)
                .collect();
            positions.sort_unstable();
            for p in positions {
                let (start, len) = thumb(track, content, viewport, p);
                assert!(
                    len >= 1 && u32::from(start) + u32::from(len) <= u32::from(track),
                    "{track} {content} {viewport} {p}"
                );
                assert!(start >= last, "the thumb moved backwards");
                last = start;
            }
            if viewport < content {
                let (start, len) = thumb(track, content, viewport, content - viewport);
                assert_eq!(
                    u32::from(start) + u32::from(len),
                    u32::from(track),
                    "the last position must reach the end"
                );
                assert_eq!(thumb(track, content, viewport, 0).0, 0);
            }
        }
    }
}

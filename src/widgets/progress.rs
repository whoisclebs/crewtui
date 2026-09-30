use super::Widget;
use crate::text::{HorizontalAlign, Line, truncate, width};
use crate::{Buffer, Rect, Style};

/// The partial block glyphs, from one eighth of a cell to seven eighths.
const EIGHTHS: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// A bar that is filled from the left in proportion to a ratio.
///
/// The bar has eighth-of-a-cell precision, so on a 10-cell bar 0.25 is two
/// full cells and half of the third. A ratio outside 0 to 1 is clamped, and
/// one that isn't a number counts as 0.
///
/// ```
/// use crewtui::widgets::{Progress, Widget};
/// use crewtui::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 10, 1);
/// let mut buf = Buffer::new(area);
/// Progress::new(0.25).label("25%").render(area, &mut buf);
/// ```
///
/// The bar fills every row of its area. The label is centered on the first
/// row, over the bar, and cut by columns if it is too wide.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress<'a> {
    ratio: f64,
    label: Option<Line<'a>>,
    filled: Style,
    empty: Style,
}

impl<'a> Progress<'a> {
    /// A bar at `ratio`, from 0.0 (empty) to 1.0 (full).
    pub fn new(ratio: f64) -> Self {
        Progress {
            ratio: if ratio.is_nan() {
                0.0
            } else {
                ratio.clamp(0.0, 1.0)
            },
            label: None,
            filled: Style::new(),
            empty: Style::new(),
        }
    }

    /// Text drawn over the bar.
    pub fn label(mut self, label: impl Into<Line<'a>>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// The style of the filled part, and of the rest.
    pub fn styles(mut self, filled: Style, empty: Style) -> Self {
        self.filled = filled;
        self.empty = empty;
        self
    }
}

impl Widget for Progress<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        let cells = usize::from(area.width);
        let eighths = (self.ratio * (cells * 8) as f64).round() as usize;
        let full = eighths / 8;
        let part = eighths % 8;
        // The partial cell is the filled color on the empty one's background.
        let part_style = self.filled.patch(Style {
            bg: self.empty.bg,
            ..Style::new()
        });
        for y in area.y..area.bottom() {
            for i in 0..cells {
                let x = area.x + i as u16;
                if i < full {
                    buf.set_string(x, y, "█", self.filled);
                } else if i == full && part > 0 {
                    buf.set_string(x, y, EIGHTHS[part - 1], part_style);
                } else {
                    buf.set_string(x, y, " ", self.empty);
                }
            }
        }
        if let Some(label) = &self.label {
            let total = label.width().min(cells);
            let offset = match label.alignment.unwrap_or(HorizontalAlign::Center) {
                HorizontalAlign::Left => 0,
                HorizontalAlign::Center => (cells - total) / 2,
                HorizontalAlign::Right => cells - total,
            };
            let mut x = area.x + offset as u16;
            let mut remaining = total;
            for span in &label.spans {
                if remaining == 0 {
                    break;
                }
                let text = truncate(&span.content, remaining);
                remaining -= width(text);
                x = buf.set_string(x, area.y, text, label.style.patch(span.style));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    fn bar(ratio: f64, w: u16) -> String {
        let area = Rect::new(0, 0, w, 1);
        let mut buf = Buffer::new(area);
        Progress::new(ratio).render(area, &mut buf);
        (0..w).map(|x| buf.get(x, 0).unwrap().symbol()).collect()
    }

    #[test]
    fn the_ends_are_exact() {
        assert_eq!(bar(0.0, 8), "        ");
        assert_eq!(bar(1.0, 8), "████████");
    }

    #[test]
    fn cells_fill_in_eighths() {
        assert_eq!(bar(0.5, 8), "████    ");
        assert_eq!(bar(0.25, 10), "██▌       ");
        assert_eq!(bar(1.0 / 8.0 / 10.0, 10), "▏         ");
        assert_eq!(bar(0.35, 10), "███▌      ");
        assert_eq!(bar(7.0 / 80.0, 10), "▉         ");
    }

    #[test]
    fn a_ratio_out_of_range_or_not_a_number_is_clamped() {
        assert_eq!(bar(-3.0, 4), "    ");
        assert_eq!(bar(7.5, 4), "████");
        assert_eq!(bar(f64::NAN, 4), "    ");
        assert_eq!(bar(f64::INFINITY, 4), "████");
        assert_eq!(bar(f64::NEG_INFINITY, 4), "    ");
    }

    #[test]
    fn the_number_of_filled_cells_never_goes_down_as_the_ratio_goes_up() {
        for w in [1u16, 3, 10, 37] {
            let mut last = 0;
            for i in 0..=1000 {
                let s = bar(f64::from(i) / 1000.0, w);
                let filled: usize = s
                    .chars()
                    .map(|c| match c {
                        '█' => 8,
                        ' ' => 0,
                        c => EIGHTHS
                            .iter()
                            .position(|e| e.starts_with(c))
                            .map_or(0, |p| p + 1),
                    })
                    .sum();
                assert!(filled >= last, "w={w} i={i}");
                last = filled;
            }
            assert_eq!(last, usize::from(w) * 8);
        }
    }

    #[test]
    fn the_label_is_centered_over_the_bar_and_cut_by_columns() {
        let area = Rect::new(0, 0, 10, 2);
        let mut buf = Buffer::new(area);
        Progress::new(0.5).label("50%").render(area, &mut buf);
        let top: String = (0..10).map(|x| buf.get(x, 0).unwrap().symbol()).collect();
        let bottom: String = (0..10).map(|x| buf.get(x, 1).unwrap().symbol()).collect();
        assert_eq!(top, "███50%    ");
        assert_eq!(bottom, "█████     ");

        let mut buf = Buffer::new(Rect::new(0, 0, 5, 1));
        Progress::new(1.0)
            .label("abcdefgh")
            .render(Rect::new(0, 0, 5, 1), &mut buf);
        let row: String = (0..5).map(|x| buf.get(x, 0).unwrap().symbol()).collect();
        assert_eq!(row, "abcde");
        let mut buf = Buffer::new(Rect::new(0, 0, 5, 1));
        Progress::new(0.0)
            .label("中文字")
            .render(Rect::new(0, 0, 5, 1), &mut buf);
        let row: String = (0..5).map(|x| buf.get(x, 0).unwrap().symbol()).collect();
        assert_eq!(row, "中文 ");
    }

    #[test]
    fn styles_apply_to_the_filled_and_empty_parts_and_the_partial_cell_blends_them() {
        let area = Rect::new(0, 0, 4, 1);
        let mut buf = Buffer::new(area);
        Progress::new(0.375)
            .styles(Style::new().fg(Color::Green), Style::new().bg(Color::Blue))
            .render(area, &mut buf);
        assert_eq!(
            buf.get(0, 0).unwrap().style(),
            Style::new().fg(Color::Green)
        );
        // Three eighths of the bar's four cells: 1 full and a half cell.
        assert_eq!(buf.get(1, 0).unwrap().symbol(), "▌");
        assert_eq!(
            buf.get(1, 0).unwrap().style(),
            Style::new().fg(Color::Green).bg(Color::Blue)
        );
        assert_eq!(buf.get(3, 0).unwrap().style(), Style::new().bg(Color::Blue));
    }

    #[test]
    fn empty_and_one_cell_areas_are_fine() {
        let mut buf = Buffer::new(Rect::new(0, 0, 3, 3));
        Progress::new(0.5)
            .label("x")
            .render(Rect::new(0, 0, 0, 0), &mut buf);
        Progress::new(0.5).render(Rect::new(9, 9, 4, 4), &mut buf);
        assert!(buf.cells().iter().all(|c| c.symbol() == " "));
        assert_eq!(bar(0.6, 1), "▋");
    }
}

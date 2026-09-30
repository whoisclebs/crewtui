use super::Widget;
use crate::{Buffer, Rect};

/// Resets every cell of its area to blank, for drawing a popup over other
/// content.
///
/// [`Block`](crate::widgets::Block) with a style only layers that style on
/// what is already there, so the text underneath shows through. Draw `Clear`
/// first, then the popup:
///
/// ```
/// use crewtui::widgets::{Block, Borders, Clear, Widget};
/// use crewtui::{Buffer, Rect};
///
/// let screen = Rect::new(0, 0, 20, 8);
/// let mut buf = Buffer::new(screen);
/// buf.set_string(0, 3, "text under the popup", crewtui::Style::new());
/// let popup = screen.centered(10, 4);
/// Clear.render(popup, &mut buf);
/// Block::new().borders(Borders::ALL).render(popup, &mut buf);
/// assert_eq!(buf.get(6, 3).unwrap().symbol(), " ");
/// ```
///
/// Text, style and hyperlinks are all removed. A wide glyph that the edge of
/// the area cuts through is blanked whole, so no half of it is left beside
/// the popup. [`Rect::centered`] places a popup in the middle of the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Clear;

impl Widget for Clear {
    fn render(self, area: Rect, buf: &mut Buffer) {
        buf.clear(area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::{Block, Borders};
    use crate::{Color, Style};

    fn row(buf: &Buffer, y: u16) -> String {
        let a = buf.area();
        (a.x..a.right())
            .map(|x| buf.get(x, y).unwrap().symbol())
            .collect()
    }

    #[test]
    fn a_styled_block_alone_lets_the_text_underneath_show() {
        let area = Rect::new(0, 0, 6, 1);
        let mut buf = Buffer::new(area);
        buf.set_string(0, 0, "abcdef", Style::new());
        Block::new()
            .style(Style::new().bg(Color::Blue))
            .render(Rect::new(1, 0, 3, 1), &mut buf);
        assert_eq!(row(&buf, 0), "abcdef");
    }

    #[test]
    fn clear_blanks_text_style_and_links_inside_the_area_only() {
        let mut buf = Buffer::new(Rect::new(0, 0, 6, 2));
        let red = Style::new().fg(Color::Red).bold();
        buf.set_string(0, 0, "abcdef", red);
        buf.set_string(0, 1, "ghijkl", red);
        buf.set_link(Rect::new(0, 0, 6, 1), Some("https://e.com"));
        Clear.render(Rect::new(1, 0, 3, 1), &mut buf);
        assert_eq!(row(&buf, 0), "a   ef");
        assert_eq!(row(&buf, 1), "ghijkl");
        let cleared = buf.get(2, 0).unwrap();
        assert_eq!(cleared.style(), Style::new());
        assert_eq!(buf.get(0, 0).unwrap().style(), red);
        assert_eq!(buf.link_at(0, 0), Some("https://e.com"));
        assert_eq!(buf.link_at(2, 0), None);
        assert_eq!(buf.link_at(4, 0), Some("https://e.com"));
    }

    #[test]
    fn a_wide_glyph_cut_by_the_left_edge_is_blanked_whole() {
        let mut buf = Buffer::new(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "a中文b", Style::new());
        // Columns: a=0, 中=1..2, 文=3..4, b=5. Clearing from 2 cuts 中.
        Clear.render(Rect::new(2, 0, 4, 1), &mut buf);
        assert_eq!(row(&buf, 0), "a     ");
        assert!(buf.cells().iter().all(|c| !c.is_continuation()));
    }

    #[test]
    fn a_wide_glyph_cut_by_the_right_edge_is_blanked_whole() {
        let mut buf = Buffer::new(Rect::new(0, 0, 6, 1));
        buf.set_string(0, 0, "a中文b", Style::new());
        // Clearing columns 0..=3 cuts 文, whose second half is column 4.
        Clear.render(Rect::new(0, 0, 4, 1), &mut buf);
        assert_eq!(row(&buf, 0), "     b");
        assert!(buf.cells().iter().all(|c| !c.is_continuation()));
    }

    #[test]
    fn clear_is_clipped_to_the_buffer_and_an_empty_area_is_fine() {
        let mut buf = Buffer::new(Rect::new(0, 0, 3, 1));
        buf.set_string(0, 0, "abc", Style::new());
        Clear.render(Rect::new(0, 0, 0, 5), &mut buf);
        assert_eq!(row(&buf, 0), "abc");
        buf.clear(Rect::new(2, 0, 50, 50));
        assert_eq!(row(&buf, 0), "ab ");
    }

    #[test]
    fn a_popup_over_text_leaves_no_trace_of_it() {
        let screen = Rect::new(0, 0, 12, 5);
        let mut buf = Buffer::new(screen);
        for y in 0..5 {
            buf.set_string(0, y, "xxxxxxxxxxxx", Style::new());
        }
        let popup = screen.centered(6, 3);
        Clear.render(popup, &mut buf);
        Block::new().borders(Borders::ALL).render(popup, &mut buf);
        assert_eq!(row(&buf, 0), "xxxxxxxxxxxx");
        assert_eq!(row(&buf, 1), "xxx┌────┐xxx");
        assert_eq!(row(&buf, 2), "xxx│    │xxx");
        assert_eq!(row(&buf, 3), "xxx└────┘xxx");
    }
}

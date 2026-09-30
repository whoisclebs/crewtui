use std::ops::{BitOr, BitOrAssign};

use super::Widget;
use crate::layout::Edges;
use crate::text::{HorizontalAlign, Line, truncate, width};
use crate::{Buffer, Rect, Style};

/// Which sides of a [`Block`] have a border.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Borders(u8);

impl Borders {
    /// No border.
    pub const NONE: Borders = Borders(0);
    /// The top edge.
    pub const TOP: Borders = Borders(1);
    /// The right edge.
    pub const RIGHT: Borders = Borders(1 << 1);
    /// The bottom edge.
    pub const BOTTOM: Borders = Borders(1 << 2);
    /// The left edge.
    pub const LEFT: Borders = Borders(1 << 3);
    /// All four edges.
    pub const ALL: Borders = Borders(0b1111);

    /// True when every edge in `other` is present.
    pub const fn contains(self, other: Borders) -> bool {
        self.0 & other.0 == other.0
    }
}

impl BitOr for Borders {
    type Output = Borders;
    fn bitor(self, rhs: Borders) -> Borders {
        Borders(self.0 | rhs.0)
    }
}

impl BitOrAssign for Borders {
    fn bitor_assign(&mut self, rhs: Borders) {
        self.0 |= rhs.0;
    }
}

/// The line characters a [`Block`] draws its border with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderType {
    /// `┌─┐│└┘`
    #[default]
    Plain,
    /// `╭─╮│╰╯`
    Rounded,
    /// `┏━┓┃┗┛`
    Thick,
    /// `╔═╗║╚╝`
    Double,
}

struct Glyphs {
    horizontal: &'static str,
    vertical: &'static str,
    top_left: &'static str,
    top_right: &'static str,
    bottom_left: &'static str,
    bottom_right: &'static str,
}

impl BorderType {
    fn glyphs(self) -> Glyphs {
        match self {
            BorderType::Plain => Glyphs {
                horizontal: "─",
                vertical: "│",
                top_left: "┌",
                top_right: "┐",
                bottom_left: "└",
                bottom_right: "┘",
            },
            BorderType::Rounded => Glyphs {
                horizontal: "─",
                vertical: "│",
                top_left: "╭",
                top_right: "╮",
                bottom_left: "╰",
                bottom_right: "╯",
            },
            BorderType::Thick => Glyphs {
                horizontal: "━",
                vertical: "┃",
                top_left: "┏",
                top_right: "┓",
                bottom_left: "┗",
                bottom_right: "┛",
            },
            BorderType::Double => Glyphs {
                horizontal: "═",
                vertical: "║",
                top_left: "╔",
                top_right: "╗",
                bottom_left: "╚",
                bottom_right: "╝",
            },
        }
    }
}

/// A frame around some content: borders, a title, a background and padding.
///
/// A block draws only the frame. The content goes in [`Block::inner`], which
/// is the area left once borders and padding are taken off, so nothing has
/// to do the border arithmetic by hand:
///
/// ```
/// use crewtui::widgets::{Block, Paragraph, Widget};
/// use crewtui::{Buffer, Rect};
///
/// let area = Rect::new(0, 0, 12, 4);
/// let mut buf = Buffer::new(area);
/// let block = Block::bordered().title("log");
/// let inner = block.inner(area);
/// block.render(area, &mut buf);
/// Paragraph::new("hello").render(inner, &mut buf);
/// ```
///
/// [`Paragraph::block`](crate::widgets::Paragraph::block) does both in one step.
///
/// The title is drawn on the top border, so it shows only when the block has
/// a top border. A title wider than the border is cut by columns.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Block<'a> {
    borders: Borders,
    border_type: BorderType,
    border_style: Style,
    style: Style,
    title: Option<Line<'a>>,
    padding: Edges,
}

impl<'a> Block<'a> {
    /// A block with no borders, which only fills its area and pads its content.
    pub fn new() -> Self {
        Block::default()
    }

    /// A block with a border on every side.
    pub fn bordered() -> Self {
        Block::new().borders(Borders::ALL)
    }

    /// Which sides have a border.
    pub fn borders(mut self, borders: Borders) -> Self {
        self.borders = borders;
        self
    }

    /// The characters the border is drawn with.
    pub fn border_type(mut self, border_type: BorderType) -> Self {
        self.border_type = border_type;
        self
    }

    /// The style of the border cells, over the block's style.
    pub fn border_style(mut self, style: Style) -> Self {
        self.border_style = style;
        self
    }

    /// The style of the whole area, borders and content included.
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// A title on the top border. Its alignment (left unless it says
    /// otherwise) places it along the edge, and its style is layered over the
    /// border's.
    pub fn title(mut self, title: impl Into<Line<'a>>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Space kept free inside the borders.
    pub fn padding(mut self, padding: impl Into<Edges>) -> Self {
        self.padding = padding.into();
        self
    }

    /// The area for the content: `area` without the borders and the padding.
    /// It is empty, and inside `area`, when they leave no room.
    pub fn inner(&self, area: Rect) -> Rect {
        let has = |b| u16::from(self.borders.contains(b));
        let edges = Edges {
            top: has(Borders::TOP).saturating_add(self.padding.top),
            right: has(Borders::RIGHT).saturating_add(self.padding.right),
            bottom: has(Borders::BOTTOM).saturating_add(self.padding.bottom),
            left: has(Borders::LEFT).saturating_add(self.padding.left),
        };
        edges.shrink(area)
    }

    fn draw_title(&self, buf: &mut Buffer, area: Rect) {
        let Some(title) = &self.title else { return };
        if !self.borders.contains(Borders::TOP) {
            return;
        }
        let left = u16::from(self.borders.contains(Borders::LEFT));
        let right = u16::from(self.borders.contains(Borders::RIGHT));
        let room = usize::from(area.width.saturating_sub(left + right));
        if room == 0 {
            return;
        }
        let base = self.style.patch(self.border_style).patch(title.style);
        // Only the text's columns count: the spans are laid out one after
        // the other and the whole is cut to the room there is.
        let total = title.width();
        let shown = total.min(room);
        let free = room - shown;
        let offset = match title.alignment.unwrap_or(HorizontalAlign::Left) {
            HorizontalAlign::Left => 0,
            HorizontalAlign::Center => free / 2,
            HorizontalAlign::Right => free,
        };
        let mut x = area.x.saturating_add(left).saturating_add(offset as u16);
        let mut remaining = shown;
        for span in &title.spans {
            if remaining == 0 {
                break;
            }
            let text = truncate(&span.content, remaining);
            remaining -= width(text);
            x = buf.set_string(x, area.y, text, base.patch(span.style));
        }
    }
}

impl Widget for Block<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        buf.set_style(area, self.style);
        let glyphs = self.border_type.glyphs();
        let style = self.style.patch(self.border_style);
        let (top, right, bottom, left) = (
            self.borders.contains(Borders::TOP),
            self.borders.contains(Borders::RIGHT),
            self.borders.contains(Borders::BOTTOM),
            self.borders.contains(Borders::LEFT),
        );
        let last_x = area.right() - 1;
        let last_y = area.bottom() - 1;

        if top {
            for x in area.x..=last_x {
                let glyph = if x == area.x && left {
                    glyphs.top_left
                } else if x == last_x && right {
                    glyphs.top_right
                } else {
                    glyphs.horizontal
                };
                buf.set_string(x, area.y, glyph, style);
            }
        }
        if bottom && !(top && last_y == area.y) {
            for x in area.x..=last_x {
                let glyph = if x == area.x && left {
                    glyphs.bottom_left
                } else if x == last_x && right {
                    glyphs.bottom_right
                } else {
                    glyphs.horizontal
                };
                buf.set_string(x, last_y, glyph, style);
            }
        }
        // The sides run between the top and bottom edges where those exist.
        let first_side = area.y + u16::from(top);
        let end_side = if bottom && last_y != area.y {
            last_y
        } else {
            area.bottom()
        };
        for y in first_side..end_side {
            if left {
                buf.set_string(area.x, y, glyphs.vertical, style);
            }
            if right && !(left && last_x == area.x) {
                buf.set_string(last_x, y, glyphs.vertical, style);
            }
        }
        self.draw_title(buf, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::Span;
    use crate::widgets::Paragraph;
    use crate::{Color, Frame};

    fn rows_of(buf: &Buffer) -> Vec<String> {
        let a = buf.area();
        (a.y..a.bottom())
            .map(|y| {
                (a.x..a.right())
                    .map(|x| buf.get(x, y).unwrap().symbol())
                    .collect()
            })
            .collect()
    }

    fn draw(block: Block<'_>, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::new(area);
        block.render(area, &mut buf);
        rows_of(&buf)
    }

    #[test]
    fn every_border_type_draws_its_own_characters() {
        let cases = [
            (BorderType::Plain, ["┌──┐", "│  │", "└──┘"]),
            (BorderType::Rounded, ["╭──╮", "│  │", "╰──╯"]),
            (BorderType::Thick, ["┏━━┓", "┃  ┃", "┗━━┛"]),
            (BorderType::Double, ["╔══╗", "║  ║", "╚══╝"]),
        ];
        for (kind, want) in cases {
            assert_eq!(
                draw(Block::bordered().border_type(kind), 4, 3),
                want,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn a_block_without_borders_draws_nothing_but_its_style() {
        let area = Rect::new(0, 0, 3, 2);
        let mut buf = Buffer::new(area);
        Block::new()
            .style(Style::new().bg(Color::Blue))
            .render(area, &mut buf);
        assert_eq!(rows_of(&buf), ["   ", "   "]);
        assert_eq!(buf.get(2, 1).unwrap().style(), Style::new().bg(Color::Blue));
    }

    #[test]
    fn edges_without_a_neighbour_run_through_the_corner() {
        assert_eq!(
            draw(Block::new().borders(Borders::TOP), 4, 3),
            ["────", "    ", "    "]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::BOTTOM), 4, 3),
            ["    ", "    ", "────"]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::LEFT), 3, 3),
            ["│  ", "│  ", "│  "]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::RIGHT), 3, 3),
            ["  │", "  │", "  │"]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::TOP | Borders::LEFT), 4, 3),
            ["┌───", "│   ", "│   "]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::LEFT | Borders::RIGHT), 4, 2),
            ["│  │", "│  │"]
        );
        assert_eq!(
            draw(Block::new().borders(Borders::TOP | Borders::BOTTOM), 3, 3),
            ["───", "   ", "───"]
        );
    }

    #[test]
    fn tiny_areas_do_not_panic_and_stay_inside() {
        for (w, h) in [
            (0, 0),
            (1, 0),
            (0, 3),
            (1, 1),
            (1, 4),
            (5, 1),
            (2, 2),
            (2, 1),
            (1, 2),
        ] {
            for borders in [
                Borders::ALL,
                Borders::TOP,
                Borders::LEFT | Borders::RIGHT,
                Borders::NONE,
            ] {
                let area = Rect::new(2, 1, w, h);
                let mut buf = Buffer::new(Rect::new(0, 0, 8, 6));
                Block::new()
                    .borders(borders)
                    .title("title")
                    .padding(1)
                    .render(area, &mut buf);
                for y in 0..6u16 {
                    for x in 0..8u16 {
                        if !area.contains(x, y) {
                            assert_eq!(
                                buf.get(x, y).unwrap().symbol(),
                                " ",
                                "({x},{y}) for {w}x{h}"
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(draw(Block::bordered(), 2, 2), ["┌┐", "└┘"]);
        assert_eq!(draw(Block::bordered(), 5, 1), ["┌───┐"]);
        assert_eq!(draw(Block::bordered(), 1, 3), ["┌", "│", "└"]);
    }

    #[test]
    fn a_title_sits_on_the_top_border_by_alignment() {
        assert_eq!(
            draw(Block::bordered().title("ab"), 8, 2),
            ["┌ab────┐", "└──────┘"]
        );
        assert_eq!(
            draw(
                Block::bordered().title(Line::raw("ab").align(HorizontalAlign::Center)),
                8,
                2
            ),
            ["┌──ab──┐", "└──────┘"]
        );
        assert_eq!(
            draw(
                Block::bordered().title(Line::raw("ab").align(HorizontalAlign::Right)),
                8,
                2
            ),
            ["┌────ab┐", "└──────┘"]
        );
    }

    #[test]
    fn a_title_too_wide_is_cut_by_columns_not_bytes() {
        assert_eq!(
            draw(Block::bordered().title("abcdefghij"), 6, 2)[0],
            "┌abcd┐"
        );
        // A wide glyph that would straddle the edge is left out.
        assert_eq!(draw(Block::bordered().title("ab中文"), 6, 2)[0], "┌ab中┐");
        // The cell the glyph would need is left as border.
        assert_eq!(draw(Block::bordered().title("中文"), 5, 2)[0], "┌中─┐");
        assert_eq!(draw(Block::bordered().title("x"), 2, 2)[0], "┌┐");
    }

    #[test]
    fn a_title_needs_a_top_border() {
        assert_eq!(
            draw(Block::new().borders(Borders::LEFT).title("ab"), 4, 2),
            ["│   ", "│   "]
        );
        // With only a top border it may use the full width.
        assert_eq!(
            draw(Block::new().borders(Borders::TOP).title("ab"), 4, 1),
            ["ab──"]
        );
    }

    #[test]
    fn title_and_border_styles_layer_over_the_block_style() {
        let area = Rect::new(0, 0, 8, 3);
        let mut buf = Buffer::new(area);
        let title = Line::from(vec![
            Span::raw("a"),
            Span::styled("b", Style::new().fg(Color::Red)),
        ])
        .style(Style::new().bold());
        Block::bordered()
            .style(Style::new().bg(Color::Blue))
            .border_style(Style::new().fg(Color::Green))
            .title(title)
            .render(area, &mut buf);
        let corner = buf.get(0, 0).unwrap().style();
        assert_eq!(corner, Style::new().bg(Color::Blue).fg(Color::Green));
        assert_eq!(
            buf.get(1, 0).unwrap().style(),
            Style::new().bg(Color::Blue).fg(Color::Green).bold()
        );
        assert_eq!(buf.get(2, 0).unwrap().style().fg, Some(Color::Red));
        // The inside only gets the block's background.
        assert_eq!(buf.get(3, 1).unwrap().style(), Style::new().bg(Color::Blue));
    }

    #[test]
    fn inner_takes_off_the_borders_and_the_padding() {
        let area = Rect::new(3, 2, 20, 10);
        assert_eq!(Block::bordered().inner(area), Rect::new(4, 3, 18, 8));
        assert_eq!(Block::new().inner(area), area);
        assert_eq!(
            Block::new()
                .borders(Borders::TOP | Borders::LEFT)
                .inner(area),
            Rect::new(4, 3, 19, 9)
        );
        assert_eq!(
            Block::bordered()
                .padding(Edges::symmetric(1, 2))
                .inner(area),
            Rect::new(6, 4, 14, 6)
        );
        assert_eq!(Block::new().padding(1).inner(area), Rect::new(4, 3, 18, 8));
    }

    #[test]
    fn inner_is_empty_and_inside_when_there_is_no_room() {
        let area = Rect::new(3, 2, 3, 3);
        let inner = Block::bordered().padding(5).inner(area);
        assert!(inner.is_empty());
        assert!(
            inner.x >= area.x
                && inner.x <= area.right()
                && inner.y >= area.y
                && inner.y <= area.bottom()
        );
        assert!(Block::bordered().inner(Rect::new(0, 0, 2, 2)).is_empty());
        let huge = Block::bordered()
            .padding(Edges::all(u16::MAX))
            .inner(Rect::new(0, 0, 10, 10));
        assert!(huge.is_empty());
    }

    #[test]
    fn a_paragraph_with_a_block_draws_its_text_inside_it() {
        let area = Rect::new(0, 0, 9, 4);
        let mut buf = Buffer::new(area);
        Paragraph::new("hello world")
            .wrap(crate::widgets::Wrap::Word)
            .block(Block::bordered().title("t"))
            .render(area, &mut buf);
        assert_eq!(
            rows_of(&buf),
            ["┌t──────┐", "│hello  │", "│world  │", "└───────┘"]
        );
    }

    #[test]
    fn a_block_around_content_leaves_the_content_cells_alone() {
        let area = Rect::new(0, 0, 6, 3);
        let mut buf = Buffer::new(area);
        buf.set_string(1, 1, "abcd", Style::new());
        Block::bordered().render(area, &mut buf);
        assert_eq!(rows_of(&buf), ["┌────┐", "│abcd│", "└────┘"]);
    }

    #[test]
    fn frame_render_widget_takes_a_block() {
        let area = Rect::new(0, 0, 5, 3);
        let mut buf = Buffer::new(area);
        Frame::new(&mut buf).render_widget(Block::bordered(), Rect::new(0, 0, 50, 50));
        assert_eq!(rows_of(&buf), ["┌───┐", "│   │", "└───┘"]);
    }

    #[test]
    fn borders_combine_and_report_what_they_contain() {
        let b = Borders::TOP | Borders::LEFT;
        assert!(
            b.contains(Borders::TOP) && b.contains(Borders::LEFT) && !b.contains(Borders::RIGHT)
        );
        assert!(Borders::ALL.contains(b));
        let mut c = Borders::NONE;
        c |= Borders::BOTTOM;
        assert_eq!(c, Borders::BOTTOM);
    }
}

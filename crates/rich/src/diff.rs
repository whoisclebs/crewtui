use crewtui::text::{Line, Span, Text};
use crewtui::widgets::{Block, Paragraph, Widget};
use crewtui::{Buffer, Color, Rect, Style};

/// The styles [`Diff`] gives to each kind of line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiffStyles {
    /// Lines that start with `+`.
    pub added: Style,
    /// Lines that start with `-`.
    pub removed: Style,
    /// Hunk headers, `@@ -1,3 +1,4 @@`.
    pub hunk: Style,
    /// File headers: `diff --git`, `index`, `---` and `+++`.
    pub header: Style,
    /// Unchanged lines and everything else.
    pub context: Style,
    /// The line numbers.
    pub gutter: Style,
}

impl Default for DiffStyles {
    fn default() -> Self {
        DiffStyles {
            added: Style::new().fg(Color::Green),
            removed: Style::new().fg(Color::Red),
            hunk: Style::new().fg(Color::Cyan),
            header: Style::new().bold(),
            context: Style::new(),
            gutter: Style::new().fg(Color::BrightBlack),
        }
    }
}

/// A unified diff, colored, with optional old and new line numbers.
///
/// It reads what `git diff` and `diff -u` write. Lines before the first
/// hunk are file headers, a line that starts with `@@` starts a hunk, and
/// inside a hunk `+`, `-` and a space say what a line is. The numbers come
/// from the hunk headers. Anything it can't place is shown as it is.
///
/// ```
/// use crewtui_rich::Diff;
///
/// let text = Diff::new("--- a\n+++ b\n@@ -1,2 +1,2 @@\n keep\n-old\n+new\n").to_text();
/// assert_eq!(text.lines.len(), 6);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diff<'a> {
    source: &'a str,
    line_numbers: bool,
    scroll: usize,
    block: Option<Block<'a>>,
    styles: DiffStyles,
}

impl<'a> Diff<'a> {
    /// A view of the diff in `source`.
    pub fn new(source: &'a str) -> Self {
        Diff {
            source,
            line_numbers: false,
            scroll: 0,
            block: None,
            styles: DiffStyles::default(),
        }
    }

    /// Whether to show the old and new line number of each line.
    pub fn line_numbers(mut self, on: bool) -> Self {
        self.line_numbers = on;
        self
    }

    /// Lines skipped from the top.
    pub fn scroll(mut self, lines: usize) -> Self {
        self.scroll = lines;
        self
    }

    /// A block drawn around the diff.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The styles of the lines.
    pub fn styles(mut self, styles: DiffStyles) -> Self {
        self.styles = styles;
        self
    }

    /// The diff as text, one line for each line of the source.
    pub fn to_text(&self) -> Text<'static> {
        let s = &self.styles;
        let rows = classify(self.source);
        // Wide enough for the biggest number, so the columns line up.
        let biggest = rows
            .iter()
            .flat_map(|r| [r.old, r.new])
            .flatten()
            .max()
            .unwrap_or(0);
        let digits = biggest.max(1).to_string().len();
        let cell = |n: Option<usize>| n.map_or(" ".repeat(digits), |n| format!("{n:>digits$}"));
        let lines = rows
            .iter()
            .map(|row| {
                let mut spans = Vec::new();
                if self.line_numbers {
                    spans.push(Span::styled(
                        format!("{} {} ", cell(row.old), cell(row.new)),
                        s.gutter,
                    ));
                }
                let style = match row.kind {
                    Kind::Header => s.header,
                    Kind::Hunk => s.hunk,
                    Kind::Added => s.added,
                    Kind::Removed => s.removed,
                    Kind::Context => s.context,
                };
                spans.push(Span::styled(row.text.to_owned(), style));
                Line {
                    spans,
                    ..Line::default()
                }
            })
            .collect();
        Text {
            lines,
            style: Style::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Header,
    Hunk,
    Added,
    Removed,
    Context,
}

struct Row<'a> {
    text: &'a str,
    kind: Kind,
    old: Option<usize>,
    new: Option<usize>,
}

/// What each line of `source` is, and its old and new line numbers.
fn classify(source: &str) -> Vec<Row<'_>> {
    let mut in_hunk = false;
    let (mut old, mut new) = (0usize, 0usize);
    source
        .lines()
        .map(|text| {
            let plain = |kind| Row {
                text,
                kind,
                old: None,
                new: None,
            };
            if let Some((o, n)) = hunk_start(text) {
                in_hunk = true;
                (old, new) = (o, n);
                plain(Kind::Hunk)
            } else if text.starts_with("diff ") || (!in_hunk && is_file_header(text)) {
                in_hunk = false;
                plain(Kind::Header)
            } else if in_hunk && text.starts_with('+') {
                new += 1;
                Row {
                    new: Some(new - 1),
                    ..plain(Kind::Added)
                }
            } else if in_hunk && text.starts_with('-') {
                old += 1;
                Row {
                    old: Some(old - 1),
                    ..plain(Kind::Removed)
                }
            } else if in_hunk && (text.starts_with(' ') || text.is_empty()) {
                old += 1;
                new += 1;
                Row {
                    old: Some(old - 1),
                    new: Some(new - 1),
                    ..plain(Kind::Context)
                }
            } else {
                // `\ No newline at end of file`, `index ...`, and the rest.
                plain(if in_hunk { Kind::Context } else { Kind::Header })
            }
        })
        .collect()
}

/// `@@ -old,count +new,count @@ ...`: where the old and new numbering start.
fn hunk_start(line: &str) -> Option<(usize, usize)> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(' ')?;
    let new = rest.strip_prefix('+')?.split(' ').next()?;
    let first = |s: &str| s.split(',').next()?.parse().ok();
    Some((first(old)?, first(new)?))
}

fn is_file_header(line: &str) -> bool {
    [
        "--- ",
        "+++ ",
        "index ",
        "new file",
        "deleted file",
        "similarity",
        "rename ",
        "old mode",
        "new mode",
        "Binary files",
    ]
    .iter()
    .any(|p| line.starts_with(p))
}

impl Widget for Diff<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let text = self.to_text();
        let mut paragraph = Paragraph::new(text).scroll(self.scroll);
        if let Some(block) = self.block {
            paragraph = paragraph.block(block);
        }
        paragraph.render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/x.rs b/x.rs\nindex 111..222 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -8,3 +8,4 @@ fn f()\n keep\n-old\n+new\n+more\n tail\n";

    fn spans(text: &Text<'static>, i: usize) -> String {
        text.lines[i]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn style_of(text: &Text<'static>, i: usize) -> Style {
        text.lines[i].spans.last().unwrap().style
    }

    #[test]
    fn each_kind_of_line_gets_its_style() {
        let s = DiffStyles::default();
        let t = Diff::new(DIFF).to_text();
        let kinds: Vec<Style> = (0..t.lines.len()).map(|i| style_of(&t, i)).collect();
        assert_eq!(
            kinds,
            [
                s.header, s.header, s.header, s.header, s.hunk, s.context, s.removed, s.added,
                s.added, s.context
            ]
        );
        // The text is the source, line for line.
        let back: Vec<String> = (0..t.lines.len()).map(|i| spans(&t, i)).collect();
        assert_eq!(back.join("\n") + "\n", DIFF);
    }

    #[test]
    fn line_numbers_follow_the_hunk_header() {
        let t = Diff::new(DIFF).line_numbers(true).to_text();
        let rows: Vec<String> = (0..t.lines.len()).map(|i| spans(&t, i)).collect();
        assert_eq!(rows[5], " 8  8  keep");
        assert_eq!(rows[6], " 9    -old");
        assert_eq!(rows[7], "    9 +new");
        assert_eq!(rows[8], "   10 +more");
        assert_eq!(rows[9], "10 11  tail");
        // Headers and the hunk line have blank number columns.
        assert!(rows[4].starts_with("      @@"), "{:?}", rows[4]);
    }

    #[test]
    fn the_number_columns_are_as_wide_as_the_biggest_number() {
        let t = Diff::new("@@ -98,2 +98,2 @@\n a\n b\n c\n")
            .line_numbers(true)
            .to_text();
        assert_eq!(spans(&t, 1), " 98  98  a");
        assert_eq!(spans(&t, 3), "100 100  c");
    }

    #[test]
    fn a_second_hunk_restarts_the_numbers_and_a_second_file_the_headers() {
        let src = "--- a\n+++ b\n@@ -1 +1 @@\n-x\n+y\ndiff --git a/z b/z\n--- a/z\n+++ b/z\n@@ -5 +5 @@\n-p\n+q\n";
        let t = Diff::new(src).line_numbers(true).to_text();
        let s = DiffStyles::default();
        assert_eq!(style_of(&t, 5), s.header);
        assert_eq!(style_of(&t, 6), s.header, "--- after a file header");
        assert!(
            spans(&t, 9).trim_start().starts_with("5"),
            "{:?}",
            spans(&t, 9)
        );
    }

    #[test]
    fn a_line_that_starts_with_dashes_inside_a_hunk_is_a_removal() {
        let s = DiffStyles::default();
        let t = Diff::new("@@ -1,2 +1 @@\n--- not a header\n+++ not a header\n").to_text();
        assert_eq!(style_of(&t, 1), s.removed);
        assert_eq!(style_of(&t, 2), s.added);
    }

    #[test]
    fn odd_input_is_shown_as_it_is() {
        let s = DiffStyles::default();
        let t = Diff::new("just words\n@@ garbage @@\n\\ No newline at end of file\n\n").to_text();
        assert_eq!(t.lines.len(), 4);
        assert_eq!(spans(&t, 0), "just words");
        assert_eq!(style_of(&t, 1), s.header);
        assert_eq!(Diff::new("").to_text().lines.len(), 0);
        // Numbers with nothing to number.
        let _ = Diff::new("").line_numbers(true).to_text();
        let _ = Diff::new("@@ -x +y @@\n+a").line_numbers(true).to_text();
    }

    #[test]
    fn it_draws_and_scrolls_and_survives_small_areas() {
        let mut buf = Buffer::new(Rect::new(0, 0, 20, 3));
        Diff::new(DIFF)
            .scroll(4)
            .render(Rect::new(0, 0, 20, 3), &mut buf);
        let row: String = (0..20).map(|x| buf.get(x, 0).unwrap().symbol()).collect();
        assert!(row.starts_with("@@ -8,3 +8,4 @@"), "{row:?}");
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 1, 1),
            Rect::new(30, 30, 5, 5),
        ] {
            Diff::new(DIFF)
                .line_numbers(true)
                .block(Block::bordered())
                .render(area, &mut buf);
        }
    }
}

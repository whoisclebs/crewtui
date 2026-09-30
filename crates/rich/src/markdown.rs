use crewtui::text::{Line, Span, Text};
use crewtui::widgets::{Paragraph, Widget, Wrap};
use crewtui::{Buffer, Color, Rect, Style};

use crate::code::{HighlightStyles, highlight};

/// The styles [`Markdown`] gives to what it renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct MarkdownStyles {
    /// Headings, levels 1 to 6.
    pub heading: [Style; 6],
    /// `*emphasis*`.
    pub emphasis: Style,
    /// `**strong**`.
    pub strong: Style,
    /// `~~strikethrough~~`.
    pub strikethrough: Style,
    /// `` `inline code` ``.
    pub code: Style,
    /// Under the lines of a fenced or indented code block.
    pub code_block: Style,
    /// The text of a link.
    pub link: Style,
    /// The bar and the text of a block quote.
    pub quote: Style,
    /// A horizontal rule.
    pub rule: Style,
    /// List markers.
    pub bullet: Style,
}

impl Default for MarkdownStyles {
    fn default() -> Self {
        MarkdownStyles {
            heading: [
                Style::new().fg(Color::Magenta).bold().underline(),
                Style::new().fg(Color::Cyan).bold(),
                Style::new().fg(Color::Yellow).bold(),
                Style::new().bold(),
                Style::new().bold(),
                Style::new().bold().dim(),
            ],
            emphasis: Style::new().italic(),
            strong: Style::new().bold(),
            strikethrough: Style::new().strikethrough(),
            code: Style::new().fg(Color::Yellow),
            code_block: Style::new(),
            link: Style::new().fg(Color::Blue).underline(),
            quote: Style::new().fg(Color::BrightBlack),
            rule: Style::new().fg(Color::BrightBlack),
            bullet: Style::new().fg(Color::Cyan),
        }
    }
}

/// Markdown as styled text.
///
/// This is a subset of CommonMark, enough for what a model or a README
/// writes: ATX headings, paragraphs with hard breaks, `*emphasis*`,
/// `**strong**`, `~~strikethrough~~`, `` `code` ``, `[links](url)` and
/// `<autolinks>`, bullet and numbered lists (nested by indentation), block
/// quotes, horizontal rules, and fenced and indented code with the syntax
/// highlighting of [`highlight`]. Links are real terminal hyperlinks. It does
/// not do tables, setext headings, reference links, HTML or images, which
/// are shown as the text they are written in.
///
/// The result does not depend on a width. Draw it with a wrapping
/// `Paragraph`, or put it in a `History`. `Markdown` is also a widget that
/// does the first of those.
///
/// ```
/// use crewtui_rich::Markdown;
///
/// let text = Markdown::new("# Title\n\nSome *emphasis* and a [link](https://example.com).").to_text();
/// assert_eq!(text.lines.len(), 3);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Markdown<'a> {
    source: &'a str,
    styles: MarkdownStyles,
    code_styles: HighlightStyles,
    rule_width: usize,
}

impl<'a> Markdown<'a> {
    /// The Markdown in `source`.
    pub fn new(source: &'a str) -> Self {
        Markdown {
            source,
            styles: MarkdownStyles::default(),
            code_styles: HighlightStyles::default(),
            rule_width: 40,
        }
    }

    /// The styles of the markup.
    pub fn styles(mut self, styles: MarkdownStyles) -> Self {
        self.styles = styles;
        self
    }

    /// The styles of the tokens in code blocks.
    pub fn code_styles(mut self, styles: HighlightStyles) -> Self {
        self.code_styles = styles;
        self
    }

    /// How many columns wide a horizontal rule is drawn. It is cut at the
    /// edge of a narrower area. 40 by default.
    pub fn rule_width(mut self, columns: usize) -> Self {
        self.rule_width = columns;
        self
    }

    /// The rendered text.
    pub fn to_text(&self) -> Text<'static> {
        let lines: Vec<&str> = self.source.lines().collect();
        let mut out = Vec::new();
        self.blocks(&lines, &mut out);
        // No blank line at the end.
        while out.last().is_some_and(is_blank) {
            out.pop();
        }
        Text {
            lines: out,
            style: Style::new(),
        }
    }

    fn blocks(&self, lines: &[&str], out: &mut Vec<Line<'static>>) {
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                if !out.is_empty() && !out.last().is_some_and(is_blank) {
                    out.push(Line::default());
                }
                i += 1;
            } else if let Some((fence, lang)) = fence_start(line) {
                let mut code = Vec::new();
                i += 1;
                while i < lines.len() && !is_fence_end(lines[i], &fence) {
                    code.push(lines[i]);
                    i += 1;
                }
                i += 1; // the closing fence, or the end
                self.code(&code.join("\n"), &lang, out);
            } else if let Some((level, text)) = heading(line) {
                let style = self.styles.heading[level - 1];
                let spans = self.inline(text, style);
                out.push(Line {
                    spans,
                    ..Line::default()
                });
                i += 1;
            } else if is_rule(line) {
                out.push(Line::from(Span::styled(
                    "─".repeat(self.rule_width),
                    self.styles.rule,
                )));
                i += 1;
            } else if quote_body(line).is_some() {
                let start = i;
                while i < lines.len() && quote_body(lines[i]).is_some() {
                    i += 1;
                }
                let inner: Vec<&str> = lines[start..i]
                    .iter()
                    .filter_map(|l| quote_body(l))
                    .collect();
                let mut quoted = Vec::new();
                self.blocks(&inner, &mut quoted);
                for mut q in quoted {
                    q.spans.insert(0, Span::styled("│ ", self.styles.quote));
                    q.style = q.style.patch(self.styles.quote);
                    out.push(q);
                }
            } else if let Some(item) = list_item(line) {
                i += 1;
                let mut text = item.text.to_owned();
                // Lines that continue the item: indented, and not a block of
                // their own.
                while i < lines.len()
                    && !lines[i].trim().is_empty()
                    && !starts_block(lines[i])
                    && indent_of(lines[i]) > item.indent
                {
                    text.push(' ');
                    text.push_str(lines[i].trim());
                    i += 1;
                }
                let depth = item.indent / 2;
                let marker = match item.marker {
                    Marker::Bullet => ["• ", "◦ ", "▪ "][depth.min(2)].to_owned(),
                    Marker::Number(n) => format!("{n}. "),
                };
                let mut spans = vec![Span::styled(
                    format!("{}{}", "  ".repeat(depth), marker),
                    self.styles.bullet,
                )];
                spans.extend(self.inline(text.trim(), Style::new()));
                out.push(Line {
                    spans,
                    ..Line::default()
                });
            } else if indent_of(line) >= 4 && out.last().is_none_or(is_blank) {
                let mut code = Vec::new();
                while i < lines.len() && (indent_of(lines[i]) >= 4 || lines[i].trim().is_empty()) {
                    code.push(strip_indent(lines[i], 4));
                    i += 1;
                }
                while code.last().is_some_and(|l| l.trim().is_empty()) {
                    code.pop();
                }
                self.code(&code.join("\n"), "", out);
            } else {
                // A paragraph runs until a blank line or another block.
                let mut pieces: Vec<(String, bool)> = Vec::new();
                while i < lines.len() && !lines[i].trim().is_empty() {
                    if !pieces.is_empty() && starts_block(lines[i]) {
                        break;
                    }
                    let raw = lines[i];
                    let more = i + 1 < lines.len()
                        && !lines[i + 1].trim().is_empty()
                        && !starts_block(lines[i + 1]);
                    // A backslash ends a line only if nothing escapes it, and
                    // a break needs a line after it.
                    let backslashes = raw.chars().rev().take_while(|&c| c == '\\').count();
                    let slash = more && backslashes % 2 == 1;
                    let hard = more && (raw.ends_with("  ") || slash);
                    let body = raw.trim();
                    let body = if slash { &body[..body.len() - 1] } else { body };
                    pieces.push((body.trim_end().to_owned(), hard));
                    i += 1;
                }
                let mut current = String::new();
                for (k, (body, hard)) in pieces.iter().enumerate() {
                    if !current.is_empty() {
                        current.push(' ');
                    }
                    current.push_str(body);
                    if *hard || k + 1 == pieces.len() {
                        out.push(Line {
                            spans: self.inline(&current, Style::new()),
                            ..Line::default()
                        });
                        current.clear();
                    }
                }
            }
        }
    }

    fn code(&self, code: &str, lang: &str, out: &mut Vec<Line<'static>>) {
        let base = self.styles.code_block;
        for mut line in highlight(code, lang, &self.code_styles) {
            for span in &mut line.spans {
                span.style = base.patch(span.style);
            }
            out.push(line);
        }
        // A fence with nothing in it is still a fence.
        if code.is_empty() {
            out.push(Line::default());
        }
    }

    /// Inline markup in `text`, over `base`.
    fn inline(&self, text: &str, base: Style) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        self.inline_into(text, base, None, &mut spans, 0);
        merge(spans)
    }

    fn inline_into(
        &self,
        text: &str,
        base: Style,
        link: Option<&str>,
        out: &mut Vec<Span<'static>>,
        depth: usize,
    ) {
        // Markup nested this deep is not worth the stack it takes.
        if depth > 16 {
            push(out, text, base, link);
            return;
        }
        let mut literal = String::new();
        let flush = |literal: &mut String, out: &mut Vec<Span<'static>>| {
            if !literal.is_empty() {
                push(out, literal, base, link);
                literal.clear();
            }
        };
        let mut i = 0;
        let bytes = text.as_bytes();
        while i < text.len() {
            let rest = &text[i..];
            let c = rest.chars().next().unwrap_or(' ');
            match c {
                '\\' => {
                    if let Some(n) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                        literal.push(n);
                        i += 1 + n.len_utf8();
                    } else {
                        literal.push('\\');
                        i += 1;
                    }
                }
                '`' => {
                    let run = rest.chars().take_while(|&c| c == '`').count();
                    match find_closing_backticks(&rest[run..], run) {
                        Some(end) => {
                            flush(&mut literal, out);
                            let code = &rest[run..run + end];
                            let code = code
                                .strip_prefix(' ')
                                .and_then(|c| c.strip_suffix(' '))
                                .filter(|c| !c.trim().is_empty())
                                .unwrap_or(code);
                            push(out, code, base.patch(self.styles.code), link);
                            i += run + end + run;
                        }
                        None => {
                            literal.push_str(&rest[..run]);
                            i += run;
                        }
                    }
                }
                '*' | '_' | '~' => {
                    let run = rest.chars().take_while(|&x| x == c).count();
                    let width = if c == '~' { 2 } else { run.min(2) };
                    let before = text[..i].chars().next_back();
                    let opens = !(c == '_' && before.is_some_and(char::is_alphanumeric))
                        && rest[run.min(width)..]
                            .chars()
                            .next()
                            .is_some_and(|n| !n.is_whitespace())
                        && !(c == '~' && run < 2);
                    let found = opens
                        .then(|| find_closing(&text[i + width..], c, width))
                        .flatten();
                    match found {
                        Some(end) if end > 0 => {
                            flush(&mut literal, out);
                            let inner = &text[i + width..i + width + end];
                            let style = match (c, width) {
                                ('~', _) => self.styles.strikethrough,
                                (_, 2) => self.styles.strong,
                                _ => self.styles.emphasis,
                            };
                            self.inline_into(inner, base.patch(style), link, out, depth + 1);
                            i += width + end + width;
                        }
                        _ => {
                            literal.push_str(&rest[..run]);
                            i += run;
                        }
                    }
                }
                '[' => match parse_link(rest) {
                    Some((label, url, used)) => {
                        flush(&mut literal, out);
                        self.inline_into(
                            label,
                            base.patch(self.styles.link),
                            Some(url),
                            out,
                            depth + 1,
                        );
                        i += used;
                    }
                    None => {
                        literal.push('[');
                        i += 1;
                    }
                },
                '<' => match autolink(rest) {
                    Some((url, used)) => {
                        flush(&mut literal, out);
                        push(out, url, base.patch(self.styles.link), Some(url));
                        i += used;
                    }
                    None => {
                        literal.push('<');
                        i += 1;
                    }
                },
                _ => {
                    literal.push(c);
                    i += c.len_utf8();
                }
            }
            let _ = bytes;
        }
        flush(&mut literal, out);
    }
}

impl Widget for Markdown<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        Paragraph::new(self.to_text())
            .wrap(Wrap::Word)
            .render(area, buf);
    }
}

fn is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|s| s.content.is_empty())
}

fn push(out: &mut Vec<Span<'static>>, text: &str, style: Style, link: Option<&str>) {
    let mut span = Span::styled(text.to_owned(), style);
    if let Some(url) = link {
        span = span.link(url.to_owned());
    }
    out.push(span);
}

/// Joins neighbors that look the same.
fn merge(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    for span in spans {
        match out.last_mut() {
            Some(last) if last.style == span.style && last.link == span.link => {
                last.content.to_mut().push_str(&span.content);
            }
            _ => out.push(span),
        }
    }
    out
}

fn indent_of(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n += 4,
            _ => break,
        }
    }
    n
}

fn strip_indent(line: &str, n: usize) -> &str {
    let mut left = n;
    let mut at = 0;
    for (i, c) in line.char_indices() {
        if left == 0 {
            break;
        }
        match c {
            ' ' => left -= 1,
            '\t' => left = left.saturating_sub(4),
            _ => break,
        }
        at = i + c.len_utf8();
    }
    &line[at..]
}

/// A fence line: the fence itself and the language after it.
fn fence_start(line: &str) -> Option<(String, String)> {
    if indent_of(line) > 3 {
        return None;
    }
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let n = t.chars().take_while(|&x| x == c).count();
    if n < 3 {
        return None;
    }
    let info = t[n..].trim();
    // A backtick fence can't have a backtick in its info string.
    if c == '`' && info.contains('`') {
        return None;
    }
    let lang = info.split_whitespace().next().unwrap_or("");
    Some((c.to_string().repeat(n), lang.to_owned()))
}

fn is_fence_end(line: &str, fence: &str) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let t = line.trim();
    let c = fence.chars().next().unwrap_or('`');
    t.len() >= fence.len() && t.chars().all(|x| x == c)
}

fn heading(line: &str) -> Option<(usize, &str)> {
    if indent_of(line) > 3 {
        return None;
    }
    let t = line.trim_start();
    let level = t.chars().take_while(|&c| c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &t[level..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    let text = rest.trim();
    // A closing run of #.
    let stripped = text.trim_end_matches('#');
    let text = if stripped.len() < text.len() && (stripped.is_empty() || stripped.ends_with(' ')) {
        stripped.trim_end()
    } else {
        text
    };
    Some((level, text))
}

fn is_rule(line: &str) -> bool {
    if indent_of(line) > 3 {
        return false;
    }
    let t: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    let Some(c) = t.chars().next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    t.len() >= 3 && t.chars().all(|x| x == c)
}

/// What follows the `>` of a quote line.
fn quote_body(line: &str) -> Option<&str> {
    if indent_of(line) > 3 {
        return None;
    }
    let rest = line.trim_start().strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest))
}

enum Marker {
    Bullet,
    Number(u64),
}

struct Item<'a> {
    indent: usize,
    marker: Marker,
    text: &'a str,
}

fn list_item(line: &str) -> Option<Item<'_>> {
    let indent = indent_of(line);
    let t = line.trim_start();
    if is_rule(line) {
        return None;
    }
    if let Some(rest) = t
        .strip_prefix(['-', '*', '+'])
        .filter(|r| r.starts_with([' ', '\t']))
    {
        return Some(Item {
            indent,
            marker: Marker::Bullet,
            text: rest.trim_start(),
        });
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if (1..=9).contains(&digits) {
        let after = &t[digits..];
        if let Some(rest) = after
            .strip_prefix(['.', ')'])
            .filter(|r| r.starts_with([' ', '\t']))
        {
            return Some(Item {
                indent,
                marker: Marker::Number(t[..digits].parse().unwrap_or(1)),
                text: rest.trim_start(),
            });
        }
    }
    None
}

fn starts_block(line: &str) -> bool {
    fence_start(line).is_some()
        || heading(line).is_some()
        || is_rule(line)
        || quote_body(line).is_some()
        || list_item(line).is_some()
}

/// The length of the text before a run of exactly `n` backticks.
fn find_closing_backticks(text: &str, n: usize) -> Option<usize> {
    let mut i = 0;
    while i < text.len() {
        if text[i..].starts_with('`') {
            let run = text[i..].chars().take_while(|&c| c == '`').count();
            if run == n {
                return Some(i);
            }
            i += run;
        } else {
            i += text[i..].chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// The length of the text before the delimiter that closes an emphasis
/// opened with `width` copies of `c`: not after a space, and for `_` not
/// inside a word.
fn find_closing(text: &str, c: char, width: usize) -> Option<usize> {
    let delim: String = std::iter::repeat_n(c, width).collect();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(after) = rest.strip_prefix('\\') {
            i += 1 + after.chars().next().map_or(0, char::len_utf8);
        } else if rest.starts_with('`') {
            let run = rest.chars().take_while(|&x| x == '`').count();
            match find_closing_backticks(&rest[run..], run) {
                Some(end) => i += run + end + run,
                None => i += run,
            }
        } else if rest.starts_with(&delim) {
            let before = text[..i].chars().next_back();
            let after = rest[delim.len()..].chars().next();
            let ok_before = before.is_some_and(|b| !b.is_whitespace());
            let ok_after = !(c == '_' && after.is_some_and(char::is_alphanumeric));
            // A longer run of the same character is not this delimiter.
            let longer = after == Some(c) && width == 1;
            if ok_before && ok_after && !longer {
                return Some(i);
            }
            i += delim.len();
        } else {
            i += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    None
}

/// `[label](url)` at the start of `text`: the label, the URL, and how many
/// bytes it took.
fn parse_link(text: &str) -> Option<(&str, &str, usize)> {
    let mut depth = 0usize;
    let mut close = None;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let close = close?;
    let after = text[close + 1..].strip_prefix('(')?;
    let end = after.find(')')?;
    let inside = after[..end].trim();
    // A title after the URL is dropped.
    let url = inside.split_whitespace().next().unwrap_or("");
    if url.is_empty() || url.contains(['<', '>']) {
        return None;
    }
    Some((&text[1..close], url, close + 2 + end + 1))
}

/// `<https://...>` at the start of `text`.
fn autolink(text: &str) -> Option<(&str, usize)> {
    let end = text.find('>')?;
    let url = &text[1..end];
    let ok = ["http://", "https://", "mailto:", "ftp://"]
        .iter()
        .any(|p| url.starts_with(p))
        && !url.contains(char::is_whitespace);
    ok.then_some((url, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(src: &str) -> Vec<String> {
        Markdown::new(src)
            .to_text()
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    fn styles_of(src: &str, line: usize) -> Vec<(String, Style)> {
        Markdown::new(src).to_text().lines[line]
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    #[test]
    fn headings_paragraphs_and_blank_lines() {
        assert_eq!(
            render("# Title\n\nfirst line\nsecond line\n\n## Sub\ntext"),
            ["Title", "", "first line second line", "", "Sub", "text"]
        );
    }

    #[test]
    fn headings_take_their_level_style_and_drop_the_hashes() {
        let s = MarkdownStyles::default();
        for level in 1..=6 {
            let src = format!("{} Hello #", "#".repeat(level));
            assert_eq!(
                styles_of(&src, 0),
                vec![("Hello".to_owned(), s.heading[level - 1])]
            );
        }
        // Seven hashes, or no space, is text.
        assert_eq!(render("####### no"), ["####### no"]);
        assert_eq!(render("#nospace"), ["#nospace"]);
        assert_eq!(render("#"), Vec::<String>::new());
    }

    #[test]
    fn emphasis_strong_strike_and_code() {
        let s = MarkdownStyles::default();
        let got = styles_of("a *b* **c** ~~d~~ `e` f", 0);
        assert_eq!(
            got,
            vec![
                ("a ".to_owned(), Style::new()),
                ("b".to_owned(), s.emphasis),
                (" ".to_owned(), Style::new()),
                ("c".to_owned(), s.strong),
                (" ".to_owned(), Style::new()),
                ("d".to_owned(), s.strikethrough),
                (" ".to_owned(), Style::new()),
                ("e".to_owned(), s.code),
                (" f".to_owned(), Style::new()),
            ]
        );
    }

    #[test]
    fn markup_nests() {
        let s = MarkdownStyles::default();
        let got = styles_of("**bold and *both* here**", 0);
        assert_eq!(got[0], ("bold and ".to_owned(), s.strong));
        assert_eq!(got[1], ("both".to_owned(), s.strong.patch(s.emphasis)));
        assert_eq!(got[2], (" here".to_owned(), s.strong));
    }

    #[test]
    fn unmatched_markers_stay_as_text() {
        assert_eq!(
            render("2 * 3 = 6 and a_b_c and *open"),
            ["2 * 3 = 6 and a_b_c and *open"]
        );
        assert_eq!(render("snake_case_name stays"), ["snake_case_name stays"]);
        assert_eq!(render("`open code"), ["`open code"]);
        assert_eq!(render("a ** b"), ["a ** b"]);
        assert_eq!(render("~single~ tilde"), ["~single~ tilde"]);
    }

    #[test]
    fn backslash_escapes_a_marker() {
        assert_eq!(
            render(r"\*not emphasis\* and \# and \\"),
            [r"*not emphasis* and # and \"]
        );
        assert_eq!(render(r"trailing \"), [r"trailing \"]);
    }

    #[test]
    fn inline_code_keeps_its_text_as_it_is() {
        assert_eq!(
            render("`*not* [a](b)` and `` a ` b ``"),
            ["*not* [a](b) and a ` b"]
        );
    }

    #[test]
    fn links_are_hyperlinks_with_the_link_style() {
        let s = MarkdownStyles::default();
        let t = Markdown::new("see [the **docs**](https://e.com/d \"title\") now").to_text();
        let spans = &t.lines[0].spans;
        assert_eq!(spans[0].content, "see ");
        assert_eq!(spans[0].link, None);
        assert_eq!(spans[1].content, "the ");
        assert_eq!(spans[1].link.as_deref(), Some("https://e.com/d"));
        assert_eq!(spans[1].style, s.link);
        assert_eq!(spans[2].content, "docs");
        assert_eq!(spans[2].link.as_deref(), Some("https://e.com/d"));
        assert_eq!(spans[2].style, s.link.patch(s.strong));
        assert_eq!(spans[3].content, " now");
    }

    #[test]
    fn autolinks_and_things_that_only_look_like_links() {
        let t = Markdown::new("go <https://e.com/x> or [nope]( ) or [also] (x) or <b>").to_text();
        let spans = &t.lines[0].spans;
        assert_eq!(spans[1].content, "https://e.com/x");
        assert_eq!(spans[1].link.as_deref(), Some("https://e.com/x"));
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "go https://e.com/x or [nope]( ) or [also] (x) or <b>");
        assert_eq!(spans.iter().filter(|s| s.link.is_some()).count(), 1);
    }

    #[test]
    fn lists_are_bulleted_numbered_and_nested_by_indent() {
        assert_eq!(
            render(
                "- one\n  more of one\n- two\n  - nested\n    - deeper\n\n1. first\n2. second\n10) tenth"
            ),
            [
                "• one more of one",
                "• two",
                "  ◦ nested",
                "    ▪ deeper",
                "",
                "1. first",
                "2. second",
                "10. tenth"
            ]
        );
        // A rule is not a list.
        assert_eq!(render("- - -").len(), 1);
        assert_eq!(render("-not a list"), ["-not a list"]);
    }

    #[test]
    fn a_quote_gets_a_bar_and_holds_blocks() {
        assert_eq!(
            render("> quoted *text*\n> more\n>\n> - item\n\nafter"),
            ["│ quoted text more", "│ ", "│ • item", "", "after"]
        );
        assert_eq!(render("> > deep"), ["│ │ deep"]);
    }

    #[test]
    fn rules() {
        for src in ["---", "***", "___", "- - -", "  ----------"] {
            let t = Markdown::new(src).rule_width(5).to_text();
            assert_eq!(t.lines[0].spans[0].content, "─────", "{src:?}");
        }
        assert_eq!(render("--"), ["--"]);
    }

    #[test]
    fn fenced_code_is_highlighted_and_keeps_its_lines() {
        let hs = HighlightStyles::default();
        let t = Markdown::new("text\n\n```rust\nfn main() {\n\n    let x = 1;\n}\n```\nafter")
            .to_text();
        let lines: Vec<String> = t
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(
            lines,
            [
                "text",
                "",
                "fn main() {",
                "",
                "    let x = 1;",
                "}",
                "after"
            ]
        );
        assert_eq!(t.lines[2].spans[0].style, hs.keyword);
        // An unknown language, an unclosed fence, a tilde fence.
        assert_eq!(render("```nope\nlet x\n```"), ["let x"]);
        assert_eq!(
            render("```\nnever closed\nstill code"),
            ["never closed", "still code"]
        );
        assert_eq!(render("~~~\ncode\n~~~"), ["code"]);
        assert_eq!(render("````\n```\n````"), ["```"]);
        assert_eq!(render("```\n```"), Vec::<String>::new());
    }

    #[test]
    fn code_inside_a_fence_is_not_read_as_markdown() {
        assert_eq!(
            render("```\n# not a heading\n- not a list\n```"),
            ["# not a heading", "- not a list"]
        );
    }

    #[test]
    fn indented_code_after_a_blank_line() {
        assert_eq!(
            render("text\n\n    code one\n\n    code two\nafter"),
            ["text", "", "code one", "", "code two", "after"]
        );
        // Four spaces inside a paragraph is a continuation.
        assert_eq!(render("text\n    more"), ["text more"]);
    }

    #[test]
    fn hard_breaks() {
        assert_eq!(
            render("one  \ntwo\\\nthree\nfour"),
            ["one", "two", "three four"]
        );
    }

    #[test]
    fn a_table_is_shown_as_the_text_it_is() {
        assert_eq!(
            render("| a | b |\n|---|---|\n| 1 | 2 |"),
            ["| a | b | |---|---| | 1 | 2 |"]
        );
    }

    #[test]
    fn weird_and_hostile_input_never_panics_and_keeps_all_the_text() {
        let inputs = [
            "",
            "\n\n\n",
            "*",
            "**",
            "***",
            "****",
            "_",
            "__a",
            "`",
            "``",
            "[",
            "[]",
            "[](",
            "[a](",
            "[a]()",
            "[[[[[",
            "]]]]",
            "<",
            "<>",
            "<http://",
            "\\",
            "\\\\",
            ">",
            ">>>>",
            "- ",
            "1.",
            "1. ",
            "```",
            "````",
            "~~~~",
            "#",
            "# ",
            "* * *",
            "> - > - >",
            "日本語 **太字** [リンク](http://x)",
            "\t\tcode",
            "a\tb",
            "\u{0}\u{1b}[31m",
            "**\u{301}a**",
            &"*".repeat(1000),
            &"[".repeat(500),
            &"> ".repeat(300),
            &"**a ".repeat(400),
            &"- ".repeat(400),
            "[a](http://x)[b](http://y)",
            "*a**b*c**",
        ];
        for src in inputs {
            let _ = Markdown::new(src).to_text();
            let mut buf = Buffer::new(Rect::new(0, 0, 10, 4));
            Markdown::new(src).render(Rect::new(0, 0, 10, 4), &mut buf);
            let mut tiny = Buffer::new(Rect::new(0, 0, 1, 1));
            Markdown::new(src).render(Rect::new(0, 0, 1, 1), &mut tiny);
        }
    }

    #[test]
    fn the_words_survive_rendering() {
        let src = "# Head\n\nSome *very* **strong** text with `code`, a [link](https://e.com) and ~~gone~~.\n\n- a\n- b\n\n> q";
        let all: String = render(src).join(" ");
        for word in [
            "Head", "Some", "very", "strong", "text", "code", "link", "gone", "a", "b", "q",
        ] {
            assert!(all.contains(word), "{word} missing from {all:?}");
        }
        assert!(
            !all.contains('*') && !all.contains('#') && !all.contains("]("),
            "{all:?}"
        );
    }

    #[test]
    fn it_draws_wrapped_into_an_area() {
        let mut buf = Buffer::new(Rect::new(0, 0, 10, 3));
        Markdown::new("aaa bbb ccc ddd").render(Rect::new(0, 0, 10, 3), &mut buf);
        let row = |y| -> String { (0..10).map(|x| buf.get(x, y).unwrap().symbol()).collect() };
        assert_eq!(row(0).trim_end(), "aaa bbb");
        assert_eq!(row(1).trim_end(), "ccc ddd");
    }

    #[test]
    fn links_are_marked_in_the_buffer() {
        let mut buf = Buffer::new(Rect::new(0, 0, 12, 1));
        Markdown::new("go [here](https://e.com)").render(Rect::new(0, 0, 12, 1), &mut buf);
        assert_eq!(buf.link_at(0, 0), None);
        assert_eq!(buf.link_at(3, 0), Some("https://e.com"));
        assert_eq!(buf.link_at(6, 0), Some("https://e.com"));
        assert_eq!(buf.link_at(7, 0), None);
    }
}

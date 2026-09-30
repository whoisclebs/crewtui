use std::collections::HashMap;

use crewtui::text::{Line, Span, Text};
use crewtui::widgets::{Paragraph, Widget, Wrap};
use crewtui::{Buffer, Color, Rect, Style};

use crate::code::{HighlightStyles, highlight};

/// Quotes nested deeper than this are shown as the text they are written in.
const MAX_QUOTE_DEPTH: usize = 16;
/// How far past a `[` the parser looks for the `]`, past a `<` for the `>`,
/// and past a `](` for the end of the link. Longer than this is not a link.
/// The limit keeps unbalanced input from costing time that grows with the
/// square of its length.
const LABEL_WINDOW: usize = 400;
const URL_WINDOW: usize = 2048;
const AUTOLINK_WINDOW: usize = 256;
/// Markup nested deeper than this is not worth the stack it takes.
const MAX_INLINE_DEPTH: usize = 16;

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
/// `**strong**`, `***both***`, `~~strikethrough~~`, `` `code` ``,
/// `[links](url)` and `<autolinks>`, bullet and numbered lists (nested by
/// indentation), block quotes, horizontal rules, and fenced and indented
/// code with the syntax highlighting of [`highlight`]. It does not do
/// tables, setext headings, reference links, HTML or images, which are shown
/// as the text they are written in.
///
/// Links are real terminal hyperlinks, but only for `http`, `https`,
/// `mailto`, `ftp` and `file` URLs. Any other link, such as `javascript:` or
/// a relative path, is shown as its text, styled like a link, and goes
/// nowhere.
///
/// The result does not depend on a width. Draw it with a wrapping
/// `Paragraph`, or put it in a `History`. `Markdown` is also a widget that
/// does the first of those. The text keeps whatever characters the source
/// had, control characters included. `Buffer` drops them when it draws, so
/// draw it through a widget and don't print the spans yourself.
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
    rule_width: Option<usize>,
}

impl<'a> Markdown<'a> {
    /// The Markdown in `source`.
    pub fn new(source: &'a str) -> Self {
        Markdown {
            source,
            styles: MarkdownStyles::default(),
            code_styles: HighlightStyles::default(),
            rule_width: None,
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

    /// How many columns wide a horizontal rule is. Without this a rule is 40
    /// columns in [`Markdown::to_text`], and as wide as the area when the
    /// markdown is drawn as a widget.
    pub fn rule_width(mut self, columns: usize) -> Self {
        self.rule_width = Some(columns);
        self
    }

    /// The rendered text.
    pub fn to_text(&self) -> Text<'static> {
        self.render_text(self.rule_width.unwrap_or(40))
    }

    fn render_text(&self, rule_width: usize) -> Text<'static> {
        let lines: Vec<&str> = self.source.lines().collect();
        let mut out = Vec::new();
        self.blocks(&lines, &mut out, rule_width, 0);
        // No blank line at the end.
        while out.last().is_some_and(is_blank) {
            out.pop();
        }
        Text {
            lines: out,
            style: Style::new(),
        }
    }

    fn blocks(&self, lines: &[&str], out: &mut Vec<Line<'static>>, rule: usize, depth: usize) {
        // The indent of each list level that is open, for nesting.
        let mut list_stack: Vec<usize> = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if line.trim().is_empty() {
                if !out.is_empty() && !out.last().is_some_and(is_blank) {
                    out.push(Line::default());
                }
                i += 1;
                continue;
            }
            let Some(item) = list_item(line) else {
                list_stack.clear();
                if let Some((fence, lang)) = fence_start(line) {
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
                    out.push(Line {
                        spans: self.inline(text, style),
                        ..Line::default()
                    });
                    i += 1;
                } else if is_rule(line) {
                    out.push(Line::from(Span::styled("─".repeat(rule), self.styles.rule)));
                    i += 1;
                } else if depth < MAX_QUOTE_DEPTH && quote_body(line).is_some() {
                    let start = i;
                    while i < lines.len() && quote_body(lines[i]).is_some() {
                        i += 1;
                    }
                    let inner: Vec<&str> = lines[start..i]
                        .iter()
                        .filter_map(|l| quote_body(l))
                        .collect();
                    let mut quoted = Vec::new();
                    self.blocks(&inner, &mut quoted, rule, depth + 1);
                    for mut q in quoted {
                        q.spans.insert(0, Span::styled("│ ", self.styles.quote));
                        q.style = q.style.patch(self.styles.quote);
                        out.push(q);
                    }
                } else if indent_of(line) >= 4 && out.last().is_none_or(is_blank) {
                    let mut code = Vec::new();
                    while i < lines.len()
                        && (indent_of(lines[i]) >= 4 || lines[i].trim().is_empty())
                    {
                        code.push(strip_indent(lines[i], 4));
                        i += 1;
                    }
                    while code.last().is_some_and(|l| l.trim().is_empty()) {
                        code.pop();
                    }
                    self.code(&code.join("\n"), "", out);
                } else {
                    i = self.paragraph(lines, i, out);
                }
                continue;
            };
            // A list item, and the lines that continue it: indented, and not
            // a block of their own.
            i += 1;
            let mut text = item.text.to_owned();
            while i < lines.len()
                && !lines[i].trim().is_empty()
                && !starts_block(lines[i])
                && indent_of(lines[i]) > item.indent
            {
                text.push(' ');
                text.push_str(lines[i].trim());
                i += 1;
            }
            while list_stack.last().is_some_and(|&top| top > item.indent) {
                list_stack.pop();
            }
            if list_stack.last() != Some(&item.indent) {
                list_stack.push(item.indent);
            }
            let level = list_stack.len() - 1;
            let marker = match item.marker {
                Marker::Bullet => ["• ", "◦ ", "▪ "][level.min(2)].to_owned(),
                Marker::Number(n) => format!("{n}. "),
            };
            let mut spans = vec![Span::styled(
                format!("{}{}", "  ".repeat(level.min(16)), marker),
                self.styles.bullet,
            )];
            spans.extend(self.inline(text.trim(), Style::new()));
            out.push(Line {
                spans,
                ..Line::default()
            });
        }
    }

    /// A paragraph starting at `lines[i]`, which runs until a blank line or
    /// another block. Returns the index after it.
    fn paragraph(&self, lines: &[&str], mut i: usize, out: &mut Vec<Line<'static>>) -> usize {
        let mut pieces: Vec<(String, bool)> = Vec::new();
        while i < lines.len() && !lines[i].trim().is_empty() {
            if !pieces.is_empty() && starts_block(lines[i]) {
                break;
            }
            let raw = lines[i];
            let more = i + 1 < lines.len()
                && !lines[i + 1].trim().is_empty()
                && !starts_block(lines[i + 1]);
            // A backslash ends a line only if nothing escapes it, and a break
            // needs a line after it.
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
        i
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
        if depth > MAX_INLINE_DEPTH {
            push(out, text, base, link);
            return;
        }
        let mut scan = Scan::new(text);
        let mut literal = String::new();
        let flush = |literal: &mut String, out: &mut Vec<Span<'static>>| {
            if !literal.is_empty() {
                push(out, literal, base, link);
                literal.clear();
            }
        };
        let mut i = 0;
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
                    match scan.ticks_end(i + run, run) {
                        Some(end) => {
                            flush(&mut literal, out);
                            let code = &text[i + run..end];
                            let code = code
                                .strip_prefix(' ')
                                .and_then(|c| c.strip_suffix(' '))
                                .filter(|c| !c.trim().is_empty())
                                .unwrap_or(code);
                            push(out, code, base.patch(self.styles.code), link);
                            i = end + run;
                        }
                        None => {
                            literal.push_str(&rest[..run]);
                            i += run;
                        }
                    }
                }
                '*' | '_' | '~' => {
                    let run = rest.chars().take_while(|&x| x == c).count();
                    match self.emphasis(&mut scan, i, run, c) {
                        Some((lead, width, end)) => {
                            literal.extend(std::iter::repeat_n(c, lead));
                            flush(&mut literal, out);
                            let start = i + lead + width;
                            let style = match (c, width) {
                                ('~', _) => self.styles.strikethrough,
                                (_, 1) => self.styles.emphasis,
                                (_, 2) => self.styles.strong,
                                _ => self.styles.strong.patch(self.styles.emphasis),
                            };
                            self.inline_into(
                                &text[start..end],
                                base.patch(style),
                                link,
                                out,
                                depth + 1,
                            );
                            i = end + width;
                        }
                        None => {
                            literal.push_str(&rest[..run]);
                            i += run;
                        }
                    }
                }
                '[' => match parse_link(rest) {
                    Some((label, url, used)) => {
                        flush(&mut literal, out);
                        // Only URLs a terminal can open and that can't do
                        // harm become links; the label is still shown.
                        let link_url = link_ok(url).then_some(url);
                        self.inline_into(
                            label,
                            base.patch(self.styles.link),
                            link_url,
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
        }
        flush(&mut literal, out);
    }

    /// Tries to open emphasis with the run of `run` copies of `c` at `at`.
    /// On success returns how many of them stay literal before the
    /// delimiter, the width of the delimiter, and where the text that closes
    /// it starts. A run longer than the delimiter uses its last characters.
    fn emphasis(
        &self,
        scan: &mut Scan<'_>,
        at: usize,
        run: usize,
        c: char,
    ) -> Option<(usize, usize, usize)> {
        let text = scan.text;
        let before = text[..at].chars().next_back();
        let widths: &[usize] = match c {
            '~' if run >= 2 => &[2],
            '~' => return None,
            _ if run >= 3 => &[3, 2, 1],
            _ if run == 2 => &[2, 1],
            _ => &[1],
        };
        for &width in widths {
            let lead = run.min(3).max(width) - width + (run - run.min(3));
            let start = at + lead;
            let after = text[start + width..].chars().next();
            let opens = after.is_some_and(|a| !a.is_whitespace())
                && !(c == '_' && before.is_some_and(char::is_alphanumeric) && lead == 0);
            if !opens {
                continue;
            }
            if let Some(end) = scan.closer(start + width, c, width, 0) {
                if end > start + width {
                    return Some((lead, width, end));
                }
            }
        }
        None
    }
}

impl Widget for Markdown<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let text = self.render_text(self.rule_width.unwrap_or(usize::from(area.width)));
        Paragraph::new(text).wrap(Wrap::Word).render(area, buf);
    }
}

/// What the scanning for closing delimiters has learned about one text, so
/// that input with many openers and no closers is not searched to the end
/// once for each.
struct Scan<'t> {
    text: &'t str,
    /// For a delimiter character and width: the earliest start from which no
    /// closer exists. A later start has none either.
    no_closer: HashMap<(char, usize), usize>,
    /// The same for a run of backticks of some length.
    no_ticks: HashMap<usize, usize>,
}

impl<'t> Scan<'t> {
    fn new(text: &'t str) -> Self {
        Scan {
            text,
            no_closer: HashMap::new(),
            no_ticks: HashMap::new(),
        }
    }

    /// Where a run of exactly `n` backticks starts, at or after `from`.
    fn ticks_end(&mut self, from: usize, n: usize) -> Option<usize> {
        if self.no_ticks.get(&n).is_some_and(|&f| from >= f) {
            return None;
        }
        let text = self.text;
        let mut i = from;
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
        let f = self.no_ticks.entry(n).or_insert(from);
        *f = (*f).min(from);
        None
    }

    /// Where the delimiter that closes emphasis of `width` copies of `c`
    /// starts, looking from `from`: a run that is not after a space, and for
    /// `_` not before a letter. A run that could open emphasis of its own,
    /// with its closer, is skipped whole.
    fn closer(&mut self, from: usize, c: char, width: usize, depth: usize) -> Option<usize> {
        if self.no_closer.get(&(c, width)).is_some_and(|&f| from >= f) {
            return None;
        }
        let text = self.text;
        let mut i = from;
        while i < text.len() {
            let rest = &text[i..];
            if let Some(after) = rest.strip_prefix('\\') {
                i += 1 + after.chars().next().map_or(0, char::len_utf8);
            } else if rest.starts_with('`') {
                let run = rest.chars().take_while(|&x| x == '`').count();
                match self.ticks_end(i + run, run) {
                    Some(end) => i = end + run,
                    None => i += run,
                }
            } else if rest.starts_with(c) {
                let run = rest.chars().take_while(|&x| x == c).count();
                let before = text[..i].chars().next_back();
                let after = text[i + run..].chars().next();
                let alnum_before = before.is_some_and(char::is_alphanumeric);
                let alnum_after = after.is_some_and(char::is_alphanumeric);
                let can_close =
                    before.is_some_and(|b| !b.is_whitespace()) && !(c == '_' && alnum_after);
                let can_open =
                    after.is_some_and(|a| !a.is_whitespace()) && !(c == '_' && alnum_before);
                if can_close && run >= width {
                    return Some(i);
                }
                if can_open && depth < 8 && c != '~' {
                    let inner = run.min(2);
                    if let Some(end) = self.closer(i + run, c, inner, depth + 1) {
                        i = end + inner;
                        continue;
                    }
                }
                i += run;
            } else {
                i += rest.chars().next().map_or(1, char::len_utf8);
            }
        }
        let f = self.no_closer.entry((c, width)).or_insert(from);
        *f = (*f).min(from);
        None
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

/// `s` cut to at most `n` bytes, on a character boundary.
fn cut(s: &str, n: usize) -> &str {
    let mut end = n.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// `[label](url "title")` at the start of `text`: the label, the URL, and
/// how many bytes it took. Parentheses in the URL must be balanced.
fn parse_link(text: &str) -> Option<(&str, &str, usize)> {
    let window = cut(text, LABEL_WINDOW);
    let mut depth = 0usize;
    let mut close = None;
    let mut chars = window.char_indices();
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
    let dest_start = close + 2;
    if !text[close + 1..].starts_with('(') {
        return None;
    }
    let tail = cut(&text[dest_start..], URL_WINDOW);
    let trimmed = tail.trim_start_matches([' ', '\t']);
    let skipped = tail.len() - trimmed.len();
    let (url, mut at) = if let Some(inner) = trimmed.strip_prefix('<') {
        let end = inner.find('>')?;
        let url = &inner[..end];
        if url.contains(char::is_whitespace) {
            return None;
        }
        (url, skipped + 1 + end + 1)
    } else {
        let mut parens = 0usize;
        let mut end = trimmed.len();
        let mut it = trimmed.char_indices();
        while let Some((i, c)) = it.next() {
            match c {
                '\\' => {
                    it.next();
                }
                '(' => parens += 1,
                ')' if parens == 0 => {
                    end = i;
                    break;
                }
                ')' => parens -= 1,
                c if c.is_whitespace() => {
                    end = i;
                    break;
                }
                c if c.is_control() => return None,
                _ => {}
            }
        }
        (&trimmed[..end], skipped + end)
    };
    if url.is_empty() {
        return None;
    }
    // An optional title, then the closing parenthesis.
    let mut rest = tail[at..].trim_start_matches([' ', '\t']);
    at = tail.len() - rest.len();
    if let Some(quote) = rest
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '('))
    {
        let closer = if quote == '(' { ')' } else { quote };
        let mut it = rest[1..].char_indices();
        let mut end = None;
        while let Some((i, c)) = it.next() {
            if c == '\\' {
                it.next();
            } else if c == closer {
                end = Some(i);
                break;
            }
        }
        let end = end?;
        rest = rest[1 + end + 1..].trim_start_matches([' ', '\t']);
        at = tail.len() - rest.len();
    }
    if !rest.starts_with(')') {
        return None;
    }
    let used = dest_start + at + 1;
    Some((&text[1..close], url, used))
}

/// `<https://...>` at the start of `text`.
fn autolink(text: &str) -> Option<(&str, usize)> {
    let window = cut(text, AUTOLINK_WINDOW);
    let end = window.find('>')?;
    let url = &text[1..end];
    (link_ok(url) && !url.contains(char::is_whitespace)).then_some((url, end + 1))
}

/// Whether a link to `url` should be a hyperlink: a scheme a terminal can
/// open and that does not run anything.
fn link_ok(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    ["http://", "https://", "mailto:", "ftp://", "file://"]
        .iter()
        .any(|p| lower.starts_with(p))
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

    #[test]
    fn emphasis_inside_emphasis_and_runs_of_three() {
        let s = MarkdownStyles::default();
        let both = s.strong.patch(s.emphasis);
        assert_eq!(
            styles_of("*a **b** c*", 0),
            vec![
                ("a ".to_owned(), s.emphasis),
                ("b".to_owned(), s.emphasis.patch(s.strong)),
                (" c".to_owned(), s.emphasis),
            ]
        );
        assert_eq!(styles_of("***both***", 0), vec![("both".to_owned(), both)]);
        assert_eq!(
            styles_of("**a *b***", 0),
            vec![
                ("a ".to_owned(), s.strong),
                ("b".to_owned(), s.strong.patch(s.emphasis)),
            ]
        );
        // A run longer than what closes leaves the rest as text, like
        // CommonMark: `**a*` is a star and then emphasis.
        assert_eq!(
            styles_of("**a*", 0),
            vec![("*".to_owned(), Style::new()), ("a".to_owned(), s.emphasis)]
        );
    }

    #[test]
    fn a_link_url_can_hold_balanced_parentheses_and_a_title() {
        for (src, url, rest) in [
            ("[a](http://x/(y))", "http://x/(y)", ""),
            ("[a](http://x/(y) \"t\") z", "http://x/(y)", " z"),
            ("[a](<http://x/y>) z", "http://x/y", " z"),
            ("[a](http://x 'title')", "http://x", ""),
            ("[a](  http://x  )", "http://x", ""),
        ] {
            let t = Markdown::new(src).to_text();
            let spans = &t.lines[0].spans;
            assert_eq!(spans[0].content, "a", "{src}");
            assert_eq!(spans[0].link.as_deref(), Some(url), "{src}");
            let after: String = spans[1..].iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(after, rest, "{src}");
        }
        // Unbalanced, or no URL: not a link.
        for src in ["[a](http://x/(y)", "[a]()", "[a](", "[a](http://x \"open)"] {
            let t = Markdown::new(src).to_text();
            assert!(t.lines[0].spans.iter().all(|s| s.link.is_none()), "{src}");
        }
    }

    #[test]
    fn only_urls_a_terminal_can_open_become_hyperlinks() {
        for src in [
            "[a](javascript:alert(1))",
            "[a](data:text/html;base64,AAAA)",
            "[a](docs/readme.md)",
            "[a](#top)",
            "[a](//example.com)",
        ] {
            let t = Markdown::new(src).to_text();
            let span = &t.lines[0].spans[0];
            assert_eq!(span.content, "a", "{src}");
            assert_eq!(span.link, None, "{src}");
            assert_eq!(span.style, MarkdownStyles::default().link, "{src}");
        }
        for url in [
            "HTTP://e.com",
            "https://e.com/x",
            "mailto:a@e.com",
            "ftp://e.com",
            "file:///tmp/x",
        ] {
            let t = Markdown::new(&format!("[a]({url})")).to_text();
            assert_eq!(t.lines[0].spans[0].link.as_deref(), Some(url));
        }
        assert_eq!(render("<javascript:alert(1)>"), ["<javascript:alert(1)>"]);
    }

    #[test]
    fn a_rule_fills_the_area_it_is_drawn_in_and_does_not_wrap() {
        let mut buf = Buffer::new(Rect::new(0, 0, 10, 4));
        Markdown::new("---\nafter").render(Rect::new(0, 0, 10, 4), &mut buf);
        let row = |y| -> String { (0..10).map(|x| buf.get(x, y).unwrap().symbol()).collect() };
        assert_eq!(row(0), "──────────");
        assert_eq!(row(1).trim_end(), "after");
        // A width given by the caller wins.
        let mut buf = Buffer::new(Rect::new(0, 0, 10, 2));
        Markdown::new("---")
            .rule_width(3)
            .render(Rect::new(0, 0, 10, 2), &mut buf);
        assert_eq!(buf.get(2, 0).unwrap().symbol(), "─");
        assert_eq!(buf.get(3, 0).unwrap().symbol(), " ");
    }

    #[test]
    fn nesting_follows_the_indents_whatever_their_size() {
        assert_eq!(
            render("- a\n    - b\n        - c\n    - d\n- e"),
            ["• a", "  ◦ b", "    ▪ c", "  ◦ d", "• e"]
        );
        assert_eq!(render("- a\n - b\n  - c"), ["• a", "  ◦ b", "    ▪ c"]);
    }

    #[test]
    fn a_very_deep_quote_does_not_overflow_the_stack() {
        let src = format!("{}x", "> ".repeat(20_000));
        let started = std::time::Instant::now();
        let t = Markdown::new(&src).to_text();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        let text: String = t.lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            text.ends_with('x'),
            "{}",
            &text[text.len().saturating_sub(20)..]
        );
        assert_eq!(text.chars().filter(|&c| c == '│').count(), MAX_QUOTE_DEPTH);
    }

    #[test]
    fn input_with_many_openers_and_no_closers_is_not_quadratic() {
        let cases: &[&str] = &[
            "*a ", " _a", "**a ", "~~a ", "*`a ", "[", "[a", "[a](", "<", "`a ", "a\r*b\r",
            "***a ", "_a_b", "**a *b ", "> *a ", "- *a\n", "[a](b) *", "\\*a ", "*a\\ ", "`` a ` ",
        ];
        for case in cases {
            let src = case.repeat(20_000 / case.len().max(1));
            let started = std::time::Instant::now();
            let t = Markdown::new(&src).to_text();
            let took = started.elapsed();
            assert!(
                took < std::time::Duration::from_secs(3),
                "{case:?} took {took:?}"
            );
            // And nothing was lost: every word is still there.
            let text: String = t
                .lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect();
            assert!(!text.is_empty(), "{case:?}");
        }
    }
}

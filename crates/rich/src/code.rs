use crewtui::text::{Line, Span, Text};

use crate::util::expand_tabs;
use crewtui::widgets::{Block, Paragraph, Widget};
use crewtui::{Buffer, Color, Constraint, Layout, Rect, Style};

/// What a language has that the scanner looks for.
struct Language {
    names: &'static [&'static str],
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: Option<(&'static str, &'static str)>,
    quotes: &'static [char],
}

const C_LIKE_COMMENTS: Option<(&str, &str)> = Some(("/*", "*/"));

const LANGUAGES: &[Language] = &[
    Language {
        names: &["rust", "rs"],
        keywords: &[
            "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
            "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod",
            "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super",
            "trait", "true", "type", "unsafe", "use", "where", "while",
        ],
        line_comments: &["//"],
        block_comment: C_LIKE_COMMENTS,
        quotes: &['"'],
    },
    Language {
        names: &["python", "py"],
        keywords: &[
            "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
            "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
            "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise", "return",
            "True", "try", "while", "with", "yield",
        ],
        line_comments: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
    },
    Language {
        names: &["javascript", "js", "jsx", "typescript", "ts", "tsx"],
        keywords: &[
            "async",
            "await",
            "break",
            "case",
            "catch",
            "class",
            "const",
            "continue",
            "default",
            "delete",
            "do",
            "else",
            "export",
            "extends",
            "false",
            "finally",
            "for",
            "function",
            "if",
            "import",
            "in",
            "instanceof",
            "interface",
            "let",
            "new",
            "null",
            "of",
            "return",
            "static",
            "super",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "type",
            "typeof",
            "undefined",
            "var",
            "void",
            "while",
            "yield",
        ],
        line_comments: &["//"],
        block_comment: C_LIKE_COMMENTS,
        quotes: &['"', '\'', '`'],
    },
    Language {
        names: &["go", "golang"],
        keywords: &[
            "break",
            "case",
            "chan",
            "const",
            "continue",
            "default",
            "defer",
            "else",
            "false",
            "for",
            "func",
            "go",
            "goto",
            "if",
            "import",
            "interface",
            "map",
            "nil",
            "package",
            "range",
            "return",
            "select",
            "struct",
            "switch",
            "true",
            "type",
            "var",
        ],
        line_comments: &["//"],
        block_comment: C_LIKE_COMMENTS,
        quotes: &['"', '`'],
    },
    Language {
        names: &["c", "cpp", "c++", "h", "hpp", "java"],
        keywords: &[
            "auto",
            "break",
            "case",
            "catch",
            "char",
            "class",
            "const",
            "continue",
            "default",
            "do",
            "double",
            "else",
            "enum",
            "extends",
            "false",
            "float",
            "for",
            "if",
            "import",
            "int",
            "long",
            "namespace",
            "new",
            "null",
            "nullptr",
            "private",
            "public",
            "return",
            "short",
            "signed",
            "sizeof",
            "static",
            "struct",
            "switch",
            "this",
            "throw",
            "true",
            "try",
            "typedef",
            "union",
            "unsigned",
            "using",
            "void",
            "volatile",
            "while",
        ],
        line_comments: &["//"],
        block_comment: C_LIKE_COMMENTS,
        quotes: &['"', '\''],
    },
    Language {
        names: &["json", "jsonc"],
        keywords: &["true", "false", "null"],
        line_comments: &["//"],
        block_comment: C_LIKE_COMMENTS,
        quotes: &['"'],
    },
    Language {
        names: &["shell", "sh", "bash", "zsh"],
        keywords: &[
            "case", "do", "done", "elif", "else", "esac", "export", "fi", "for", "function", "if",
            "in", "local", "return", "then", "until", "while",
        ],
        line_comments: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
    },
    Language {
        names: &["toml", "yaml", "yml", "ini"],
        keywords: &["true", "false", "null"],
        line_comments: &["#"],
        block_comment: None,
        quotes: &['"', '\''],
    },
];

fn language(name: &str) -> Option<&'static Language> {
    let name = name.trim().to_ascii_lowercase();
    LANGUAGES.iter().find(|l| l.names.contains(&name.as_str()))
}

/// The styles [`highlight`] and [`CodeBlock`] give to each kind of token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HighlightStyles {
    /// Text that is none of the below.
    pub plain: Style,
    /// Keywords, and literals like `true` and `None`.
    pub keyword: Style,
    /// String and character literals.
    pub string: Style,
    /// Line and block comments.
    pub comment: Style,
    /// Numbers.
    pub number: Style,
    /// The line numbers of a [`CodeBlock`].
    pub gutter: Style,
}

impl Default for HighlightStyles {
    fn default() -> Self {
        HighlightStyles {
            plain: Style::new(),
            keyword: Style::new().fg(Color::Magenta).bold(),
            string: Style::new().fg(Color::Green),
            comment: Style::new().fg(Color::BrightBlack).italic(),
            number: Style::new().fg(Color::Yellow),
            gutter: Style::new().fg(Color::BrightBlack),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Plain,
    Keyword,
    String,
    Comment,
    Number,
}

/// Splits `code` into lines of styled spans for `language`, which is a name
/// like `rust`, `py` or `json` (case does not matter). A language the
/// scanner doesn't know, or an empty name, gives plain lines. Block comments
/// and backtick strings that run over several lines keep their style on each;
/// other strings end with their line. Tabs become spaces, four columns to a
/// stop, since a tab can't be drawn.
///
/// ```
/// use crewtui_rich::{highlight, HighlightStyles};
///
/// let lines = highlight("let x = 1; // one", "rust", &HighlightStyles::default());
/// assert_eq!(lines.len(), 1);
/// let text: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
/// assert_eq!(text, "let x = 1; // one");
/// ```
pub fn highlight(code: &str, language_name: &str, styles: &HighlightStyles) -> Vec<Line<'static>> {
    let Some(lang) = language(language_name) else {
        return code
            .lines()
            .map(|l| Line::from(Span::styled(expand_tabs(l).into_owned(), styles.plain)))
            .collect();
    };
    let mut open_block = false;
    let mut open_quote: Option<char> = None;
    code.lines()
        .map(|line| {
            let line = expand_tabs(line);
            let line = line.as_ref();
            let tokens = scan_line(line, lang, &mut open_block, &mut open_quote);
            let mut spans: Vec<Span<'static>> = Vec::new();
            let mut last: Option<Kind> = None;
            for (text, kind) in tokens {
                if last == Some(kind) {
                    if let Some(span) = spans.last_mut() {
                        span.content.to_mut().push_str(text);
                        continue;
                    }
                }
                let style = match kind {
                    Kind::Plain => styles.plain,
                    Kind::Keyword => styles.keyword,
                    Kind::String => styles.string,
                    Kind::Comment => styles.comment,
                    Kind::Number => styles.number,
                };
                spans.push(Span::styled(text.to_owned(), style));
                last = Some(kind);
            }
            Line {
                spans,
                ..Line::default()
            }
        })
        .collect()
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_'
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The pieces of one line and what each is. `open_block` and `open_quote`
/// carry a comment or a string over from the line before and to the next.
fn scan_line<'a>(
    line: &'a str,
    lang: &Language,
    open_block: &mut bool,
    open_quote: &mut Option<char>,
) -> Vec<(&'a str, Kind)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < line.len() {
        // Inside a block comment or a string from an earlier line.
        if *open_block {
            let (_, end) = lang.block_comment.unwrap_or(("", "*/"));
            match line[i..].find(end) {
                Some(at) => {
                    out.push((&line[i..i + at + end.len()], Kind::Comment));
                    i += at + end.len();
                    *open_block = false;
                }
                None => {
                    out.push((&line[i..], Kind::Comment));
                    i = line.len();
                }
            }
            continue;
        }
        if let Some(quote) = *open_quote {
            let end = string_end(line, i, quote);
            out.push((&line[i..end.0], Kind::String));
            i = end.0;
            if end.1 {
                *open_quote = None;
            }
            continue;
        }
        let rest = &line[i..];
        let Some(c) = rest.chars().next() else { break };
        if lang.line_comments.iter().any(|m| rest.starts_with(m)) {
            out.push((rest, Kind::Comment));
            break;
        }
        if let Some((start, end)) = lang.block_comment {
            if let Some(after) = rest.strip_prefix(start) {
                match after.find(end) {
                    Some(at) => {
                        let len = start.len() + at + end.len();
                        out.push((&rest[..len], Kind::Comment));
                        i += len;
                    }
                    None => {
                        out.push((rest, Kind::Comment));
                        *open_block = true;
                        i = line.len();
                    }
                }
                continue;
            }
        }
        if lang.quotes.contains(&c) {
            let (end, closed) = string_end(line, i + c.len_utf8(), c);
            out.push((&line[i..end], Kind::String));
            i = end;
            // A backtick string can go on to the next line, the others end
            // with the line.
            if !closed && c == '`' {
                *open_quote = Some(c);
            }
            continue;
        }
        if c.is_ascii_digit() {
            let mut end = i;
            for (k, ch) in rest.char_indices() {
                if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                    end = i + k + ch.len_utf8();
                } else {
                    break;
                }
            }
            // `1..2` is a range, not a number with dots.
            let text = &line[i..end];
            let cut = text.find("..").unwrap_or(text.len());
            let end = i + cut;
            out.push((&line[i..end], Kind::Number));
            i = end;
            continue;
        }
        if is_ident_start(c) {
            let mut end = i;
            for (k, ch) in rest.char_indices() {
                if is_ident(ch) {
                    end = i + k + ch.len_utf8();
                } else {
                    break;
                }
            }
            let word = &line[i..end];
            let kind = if lang.keywords.contains(&word) {
                Kind::Keyword
            } else {
                Kind::Plain
            };
            out.push((word, kind));
            i = end;
            continue;
        }
        // Anything else, a character at a time so a multi-byte one is whole.
        let len = c.len_utf8();
        out.push((&line[i..i + len], Kind::Plain));
        i += len;
    }
    out
}

/// Where the string that began before `from` ends on this line, and whether
/// it was closed. A backslash escapes the character after it.
fn string_end(line: &str, from: usize, quote: char) -> (usize, bool) {
    let mut escaped = false;
    for (k, ch) in line[from..].char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return (from + k + ch.len_utf8(), true);
        }
    }
    (line.len(), false)
}

/// Code with optional line numbers and syntax highlighting.
///
/// Lines are cut at the right edge, not wrapped, and `scroll` skips lines
/// from the top.
///
/// ```
/// use crewtui::{Buffer, Rect};
/// use crewtui::widgets::Widget;
/// use crewtui_rich::CodeBlock;
///
/// let area = Rect::new(0, 0, 20, 2);
/// let mut buf = Buffer::new(area);
/// CodeBlock::new("fn main() {}\nlet x = 1;").language("rust").line_numbers(true).render(area, &mut buf);
/// assert_eq!(buf.get(0, 0).unwrap().symbol(), "1");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock<'a> {
    code: &'a str,
    language: Option<&'a str>,
    line_numbers: bool,
    first_line: usize,
    scroll: usize,
    block: Option<Block<'a>>,
    styles: HighlightStyles,
}

impl<'a> CodeBlock<'a> {
    /// A block of `code`, plain until a language is set.
    pub fn new(code: &'a str) -> Self {
        CodeBlock {
            code,
            language: None,
            line_numbers: false,
            first_line: 1,
            scroll: 0,
            block: None,
            styles: HighlightStyles::default(),
        }
    }

    /// The language to highlight, by name.
    pub fn language(mut self, language: &'a str) -> Self {
        self.language = Some(language);
        self
    }

    /// Whether to number the lines.
    pub fn line_numbers(mut self, on: bool) -> Self {
        self.line_numbers = on;
        self
    }

    /// The number of the first line, for code that is an excerpt. One by
    /// default.
    pub fn first_line(mut self, number: usize) -> Self {
        self.first_line = number;
        self
    }

    /// Lines skipped from the top.
    pub fn scroll(mut self, lines: usize) -> Self {
        self.scroll = lines;
        self
    }

    /// A block drawn around the code.
    pub fn block(mut self, block: Block<'a>) -> Self {
        self.block = Some(block);
        self
    }

    /// The styles of the tokens and of the line numbers.
    pub fn styles(mut self, styles: HighlightStyles) -> Self {
        self.styles = styles;
        self
    }

    /// The highlighted lines, without line numbers.
    pub fn to_text(&self) -> Text<'static> {
        Text {
            lines: highlight(self.code, self.language.unwrap_or(""), &self.styles),
            style: Style::new(),
        }
    }
}

impl Widget for CodeBlock<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let area = area.intersection(buf.area());
        if area.is_empty() {
            return;
        }
        let area = match &self.block {
            Some(block) => {
                let inner = block.inner(area);
                block.clone().render(area, buf);
                inner
            }
            None => area,
        };
        if area.is_empty() {
            return;
        }
        let text = self.to_text();
        let count = text.lines.len();
        let gutter = if self.line_numbers && count > 0 {
            let last = self.first_line.saturating_add(count - 1);
            last.to_string().len() as u16 + 1
        } else {
            0
        };
        let [numbers, code] = Layout::row()
            .constraints([Constraint::Fixed(gutter), Constraint::Fill(1)])
            .split_array(area);
        if gutter > 0 {
            let width = usize::from(gutter) - 1;
            let lines: Vec<Line<'static>> = (0..count)
                .map(|i| {
                    Line::from(Span::styled(
                        format!("{:>width$}", self.first_line.saturating_add(i)),
                        self.styles.gutter,
                    ))
                })
                .collect();
            Paragraph::new(Text {
                lines,
                style: Style::new(),
            })
            .scroll(self.scroll)
            .render(numbers, buf);
        }
        Paragraph::new(text).scroll(self.scroll).render(code, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, lang: &str) -> Vec<(String, Style)> {
        let styles = HighlightStyles::default();
        highlight(line, lang, &styles)[0]
            .spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    fn plain(line: &str, lang: &str) -> String {
        highlight(line, lang, &HighlightStyles::default())
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn keywords_strings_comments_and_numbers_get_their_styles() {
        let s = HighlightStyles::default();
        let got = kinds("let x = \"hi\" + 42; // note", "rust");
        assert_eq!(
            got,
            vec![
                ("let".to_owned(), s.keyword),
                (" x = ".to_owned(), s.plain),
                ("\"hi\"".to_owned(), s.string),
                (" + ".to_owned(), s.plain),
                ("42".to_owned(), s.number),
                ("; ".to_owned(), s.plain),
                ("// note".to_owned(), s.comment),
            ]
        );
    }

    #[test]
    fn the_text_is_never_changed_whatever_the_language_or_the_input() {
        let inputs = [
            "",
            "plain words",
            "fn main() { println!(\"a\\\"b\"); }",
            "x = 'it\\'s' # comment",
            "/* open\nstill open */ let y = 1.5e3;",
            "日本語 = \"字\" // 注釈",
            "let s = \"unterminated",
            "`multi\nline` after",
            "1..2 0x1F 1_000 3.14 a1 _b",
            "\t  indented\t",
            "\"\\",
        ];
        for lang in [
            "rust", "python", "js", "go", "c", "json", "sh", "toml", "nope", "",
        ] {
            for input in inputs {
                let expanded: Vec<String> =
                    input.lines().map(|l| expand_tabs(l).into_owned()).collect();
                let want: Vec<&str> = expanded.iter().map(String::as_str).collect();
                let got = plain(input, lang);
                assert_eq!(
                    got.split('\n').collect::<Vec<_>>().len().max(1),
                    want.len().max(1),
                    "{lang} {input:?}"
                );
                assert_eq!(got, want.join("\n"), "{lang} {input:?}");
            }
        }
    }

    #[test]
    fn an_unknown_or_empty_language_is_plain() {
        let s = HighlightStyles::default();
        for lang in ["", "klingon", "  "] {
            let lines = highlight("let x = 1", lang, &s);
            assert_eq!(lines[0].spans, vec![Span::styled("let x = 1", s.plain)]);
        }
    }

    #[test]
    fn language_names_ignore_case_and_have_aliases() {
        let s = HighlightStyles::default();
        for name in ["Rust", "RS", " rust "] {
            assert_eq!(kinds("fn", name)[0].1, s.keyword, "{name}");
        }
        assert_eq!(kinds("def", "py")[0].1, s.keyword);
        assert_eq!(kinds("func", "golang")[0].1, s.keyword);
    }

    #[test]
    fn a_block_comment_keeps_its_style_over_lines() {
        let s = HighlightStyles::default();
        let lines = highlight("a /* one\ntwo\nthree */ b", "rust", &s);
        assert_eq!(lines[1].spans, vec![Span::styled("two", s.comment)]);
        assert_eq!(lines[2].spans[0], Span::styled("three */", s.comment));
        assert_eq!(lines[2].spans[1].style, s.plain);
    }

    #[test]
    fn a_comment_marker_inside_a_string_is_not_a_comment() {
        let s = HighlightStyles::default();
        let got = kinds("let u = \"http://x\";", "rust");
        assert!(
            got.iter()
                .any(|(t, st)| t == "\"http://x\"" && *st == s.string),
            "{got:?}"
        );
        assert!(!got.iter().any(|(_, st)| *st == s.comment), "{got:?}");
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let s = HighlightStyles::default();
        let got = kinds(r#"x = "a\"b" y"#, "python");
        assert!(got.contains(&(r#""a\"b""#.to_owned(), s.string)), "{got:?}");
    }

    #[test]
    fn a_range_is_not_a_number_with_dots() {
        let s = HighlightStyles::default();
        let got = kinds("0..10", "rust");
        assert_eq!(got[0], ("0".to_owned(), s.number));
        assert_eq!(got[1].0, "..");
    }

    #[test]
    fn a_code_block_numbers_its_lines_and_scrolls() {
        let area = Rect::new(0, 0, 12, 2);
        let mut buf = Buffer::new(area);
        let code = (1..=12)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        CodeBlock::new(&code)
            .line_numbers(true)
            .first_line(95)
            .scroll(3)
            .render(area, &mut buf);
        let row = |y| -> String { (0..12).map(|x| buf.get(x, y).unwrap().symbol()).collect() };
        // Numbers go up to 106, so the gutter is three wide and a space.
        assert_eq!(row(0).trim_end(), " 98 l4");
        assert_eq!(row(1).trim_end(), " 99 l5");
    }

    #[test]
    fn degenerate_areas_and_empty_code_draw_nothing_and_do_not_panic() {
        let mut buf = Buffer::new(Rect::new(0, 0, 5, 3));
        for area in [
            Rect::new(0, 0, 0, 0),
            Rect::new(0, 0, 1, 1),
            Rect::new(9, 9, 4, 4),
        ] {
            CodeBlock::new("a\nb")
                .line_numbers(true)
                .render(area, &mut buf);
            CodeBlock::new("")
                .line_numbers(true)
                .language("rust")
                .render(area, &mut buf);
        }
        CodeBlock::new("x")
            .block(Block::bordered())
            .render(Rect::new(0, 0, 5, 3), &mut buf);
    }

    #[test]
    fn code_indented_with_tabs_keeps_its_indentation() {
        let lines = highlight(
            "func f() {\n\treturn 1\n\t\tx()\n}",
            "go",
            &HighlightStyles::default(),
        );
        let text: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(text, ["func f() {", "    return 1", "        x()", "}"]);
        let plain = highlight("\tx", "", &HighlightStyles::default());
        assert_eq!(plain[0].spans[0].content, "    x");
    }

    #[test]
    fn a_first_line_number_at_the_top_of_usize_does_not_overflow() {
        let area = Rect::new(0, 0, 40, 3);
        let mut buf = Buffer::new(area);
        CodeBlock::new("a\nb\nc")
            .line_numbers(true)
            .first_line(usize::MAX)
            .render(area, &mut buf);
    }
}

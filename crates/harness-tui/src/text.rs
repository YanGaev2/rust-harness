//! Text primitives: styles, spans, lines, and Unicode-aware measurement.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Terminal color: default, 16-color palette (0-15), 256-color palette,
/// or 24-bit RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Color {
    #[default]
    Default,
    Ansi(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Text attributes for a span. `Style::default()` is plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub italic: bool,
    pub dim: bool,
    pub underline: bool,
    pub reverse: bool,
}

impl Style {
    pub fn is_plain(&self) -> bool {
        *self == Style::default()
    }
}

/// A run of text with a single style.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

impl Span {
    pub fn raw(text: impl Into<String>) -> Self {
        Span {
            text: text.into(),
            style: Style::default(),
        }
    }

    pub fn styled(text: impl Into<String>, style: Style) -> Self {
        Span {
            text: text.into(),
            style,
        }
    }
}

/// One visual row: a sequence of styled spans.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Line {
    pub spans: Vec<Span>,
}

impl Line {
    pub fn raw(text: impl Into<String>) -> Self {
        Line {
            spans: vec![Span::raw(text)],
        }
    }

    /// Visible width of the whole line in terminal columns.
    pub fn width(&self) -> usize {
        self.spans
            .iter()
            .map(|span| visible_width(&span.text))
            .sum()
    }

    /// Plain text of the line, styles stripped.
    pub fn text(&self) -> String {
        self.spans.iter().map(|span| span.text.as_str()).collect()
    }
}

/// Visible terminal width of `text`: CJK and emoji count 2 columns,
/// combining marks 0.
pub fn visible_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// One grapheme cluster with its display width and inherited style.
struct Cell<'a> {
    grapheme: &'a str,
    width: usize,
    style: Style,
}

/// Greedy word wrap. Breaks at spaces (the break space is consumed),
/// hard-breaks words longer than `width`, never splits a grapheme
/// cluster, and preserves span styles. `width == 0` disables wrapping.
pub fn wrap(line: &Line, width: usize) -> Vec<Line> {
    if width == 0 {
        return vec![line.clone()];
    }

    let cells: Vec<Cell<'_>> = line
        .spans
        .iter()
        .flat_map(|span| {
            span.text.graphemes(true).map(move |grapheme| Cell {
                grapheme,
                width: visible_width(grapheme),
                style: span.style,
            })
        })
        .collect();

    let mut lines: Vec<Vec<Cell<'_>>> = Vec::new();
    let mut current: Vec<Cell<'_>> = Vec::new();
    let mut current_width = 0usize;
    // Index in `current` of the last space we may break after.
    let mut last_break: Option<usize> = None;

    for cell in cells {
        if current_width + cell.width > width && !current.is_empty() {
            if cell.grapheme == " " {
                // Breaking exactly on the space: flush and consume it.
                lines.push(std::mem::take(&mut current));
                current_width = 0;
                last_break = None;
                continue;
            }
            if let Some(break_at) = last_break {
                // Move the word in progress down to the next line.
                let tail = current.split_off(break_at + 1);
                while current.last().is_some_and(|c| c.grapheme == " ") {
                    current.pop();
                }
                lines.push(std::mem::take(&mut current));
                current = tail;
                current_width = current.iter().map(|c| c.width).sum();
                last_break = None;
            } else {
                // No break point: hard-break the long word.
                lines.push(std::mem::take(&mut current));
                current_width = 0;
            }
        }
        if cell.grapheme == " " {
            last_break = Some(current.len());
        }
        current_width += cell.width;
        current.push(cell);
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines.into_iter().map(cells_to_line).collect()
}

impl Style {
    /// Full SGR escape sequence for this style, or an empty string for
    /// plain text. Attribute order: bold, dim, italic, underline,
    /// reverse, fg, bg.
    pub fn sgr(&self) -> String {
        if self.is_plain() {
            return String::new();
        }
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.reverse {
            codes.push("7".to_string());
        }
        push_color_codes(&mut codes, self.fg, 30, 38);
        push_color_codes(&mut codes, self.bg, 40, 48);
        format!("\x1b[{}m", codes.join(";"))
    }
}

/// `base` is 30 for foreground / 40 for background; `extended` is the
/// 38/48 introducer for indexed and RGB colors.
fn push_color_codes(codes: &mut Vec<String>, color: Color, base: u8, extended: u8) {
    match color {
        Color::Default => {}
        Color::Ansi(n) if n < 8 => codes.push((base + n).to_string()),
        Color::Ansi(n) => codes.push((base + 60 + (n - 8)).to_string()),
        Color::Indexed(n) => codes.push(format!("{extended};5;{n}")),
        Color::Rgb(r, g, b) => codes.push(format!("{extended};2;{r};{g};{b}")),
    }
}

/// Render a line to a string with ANSI styling. Each styled span is
/// wrapped `SGR .. text .. reset`; plain spans pass through untouched.
pub fn render_ansi(line: &Line) -> String {
    let mut out = String::new();
    for span in &line.spans {
        if span.style.is_plain() {
            out.push_str(&span.text);
        } else {
            out.push_str(&span.style.sgr());
            out.push_str(&span.text);
            out.push_str("\x1b[0m");
        }
    }
    out
}

/// Rebuild a line from cells, merging adjacent cells with equal style.
fn cells_to_line(cells: Vec<Cell<'_>>) -> Line {
    let mut spans: Vec<Span> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(span) if span.style == cell.style => span.text.push_str(cell.grapheme),
            _ => spans.push(Span {
                text: cell.grapheme.to_string(),
                style: cell.style,
            }),
        }
    }
    Line { spans }
}

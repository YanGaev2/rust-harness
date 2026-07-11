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

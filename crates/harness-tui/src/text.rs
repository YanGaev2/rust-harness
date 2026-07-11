//! Text primitives: styles, spans, lines, and Unicode-aware measurement.

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

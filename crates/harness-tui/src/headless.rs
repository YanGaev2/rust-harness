//! In-memory terminal for snapshot tests: apply row updates, read back
//! text, styles, and the drawn caret.

use crate::diff::RowUpdate;
use crate::text::{Color, Line, Style};

/// A fake terminal that stores the current frame. Components and the
/// core render against this in tests instead of a real terminal.
pub struct TestTerminal {
    width: u16,
    rows: Vec<Line>,
}

impl TestTerminal {
    pub fn new(width: u16) -> Self {
        TestTerminal {
            width,
            rows: Vec::new(),
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    /// Apply row updates the way a real terminal would.
    pub fn apply(&mut self, updates: &[RowUpdate]) {
        for update in updates {
            match update {
                RowUpdate::Write { row, line } => {
                    if self.rows.len() <= *row {
                        self.rows.resize(*row + 1, Line::default());
                    }
                    self.rows[*row] = line.clone();
                }
                RowUpdate::Clear { row } => {
                    if *row < self.rows.len() {
                        self.rows[*row] = Line::default();
                    }
                }
            }
        }
    }

    /// Plain-text snapshot: rows joined with newlines, styles stripped.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(Line::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Styled snapshot: styled spans render as `[text]{tags}` so tests
    /// can assert on attributes and the drawn caret (`[ ]{reverse}`).
    pub fn styled(&self) -> String {
        self.rows
            .iter()
            .map(styled_row)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn styled_row(line: &Line) -> String {
    let mut out = String::new();
    for span in &line.spans {
        if span.style.is_plain() {
            out.push_str(&span.text);
        } else {
            out.push('[');
            out.push_str(&span.text);
            out.push_str("]{");
            out.push_str(&style_tags(&span.style));
            out.push('}');
        }
    }
    out
}

/// Tags in fixed order: bold, dim, italic, underline, reverse, fg, bg.
fn style_tags(style: &Style) -> String {
    let mut tags: Vec<String> = Vec::new();
    if style.bold {
        tags.push("bold".to_string());
    }
    if style.dim {
        tags.push("dim".to_string());
    }
    if style.italic {
        tags.push("italic".to_string());
    }
    if style.underline {
        tags.push("underline".to_string());
    }
    if style.reverse {
        tags.push("reverse".to_string());
    }
    if style.fg != Color::Default {
        tags.push(format!("fg={:?}", style.fg));
    }
    if style.bg != Color::Default {
        tags.push(format!("bg={:?}", style.bg));
    }
    tags.join(",")
}

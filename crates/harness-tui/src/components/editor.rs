//! Multi-line prompt editor with a drawn caret.
//!
//! The caret is an inverse-styled cell rendered into the line data —
//! the hardware cursor stays hidden for the whole session, which is
//! what kills cursor flicker (and makes the caret snapshot-testable).

use unicode_segmentation::UnicodeSegmentation;

use crate::text::{Line, Span, Style, visible_width};

/// Editor state: a logical text buffer plus a grapheme-index caret.
pub struct Editor {
    text: String,
    /// Caret as an index into the grapheme sequence (newlines count 1).
    cursor: usize,
    prompt: String,
    placeholder: String,
    prompt_style: Style,
    placeholder_style: Style,
}

impl Editor {
    pub fn new(prompt: impl Into<String>, placeholder: impl Into<String>) -> Self {
        Editor {
            text: String::new(),
            cursor: 0,
            prompt: prompt.into(),
            placeholder: placeholder.into(),
            prompt_style: Style::default(),
            placeholder_style: Style::default().dim(),
        }
    }

    /// Text shown while the buffer is empty.
    pub fn set_placeholder(&mut self, placeholder: impl Into<String>) {
        self.placeholder = placeholder.into();
    }

    /// Colors for the prompt glyph and the empty-editor placeholder.
    pub fn set_styles(&mut self, prompt: Style, placeholder: Style) {
        self.prompt_style = prompt;
        self.placeholder_style = placeholder;
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor = self.grapheme_count();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    /// Take the buffer out, leaving the editor empty.
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }

    pub fn insert_char(&mut self, ch: char) {
        let at = self.byte_offset(self.cursor);
        self.text.insert(at, ch);
        self.cursor += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        let at = self.byte_offset(self.cursor);
        self.text.insert_str(at, s);
        self.cursor += s.graphemes(true).count();
    }

    /// Remove the grapheme before the caret.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_offset(self.cursor - 1);
        let end = self.byte_offset(self.cursor);
        self.text.replace_range(start..end, "");
        self.cursor -= 1;
    }

    /// Remove the grapheme under the caret (the Delete key).
    pub fn delete_forward(&mut self) {
        if self.cursor >= self.grapheme_count() {
            return;
        }
        let start = self.byte_offset(self.cursor);
        let end = self.byte_offset(self.cursor + 1);
        self.text.replace_range(start..end, "");
    }

    /// Remove the word before the caret plus the spaces between them.
    pub fn delete_word_back(&mut self) {
        let start = self.word_start_before(self.cursor);
        self.remove_graphemes(start, self.cursor);
        self.cursor = start;
    }

    /// Remove from the start of the caret's line to the caret.
    pub fn delete_to_line_start(&mut self) {
        let start = self.line_bounds().0;
        self.remove_graphemes(start, self.cursor);
        self.cursor = start;
    }

    /// Remove from the caret to the end of its line.
    pub fn delete_to_line_end(&mut self) {
        let end = self.line_bounds().1;
        self.remove_graphemes(self.cursor, end);
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.grapheme_count());
    }

    /// To the start of the previous word.
    pub fn move_word_left(&mut self) {
        self.cursor = self.word_start_before(self.cursor);
    }

    /// Past the end of the next word.
    pub fn move_word_right(&mut self) {
        let graphemes: Vec<&str> = self.text.graphemes(true).collect();
        let mut at = self.cursor;
        while at < graphemes.len() && is_space(graphemes[at]) {
            at += 1;
        }
        while at < graphemes.len() && !is_space(graphemes[at]) {
            at += 1;
        }
        self.cursor = at;
    }

    /// To the start of the caret's line.
    pub fn move_home(&mut self) {
        self.cursor = self.line_bounds().0;
    }

    /// To the end of the caret's line.
    pub fn move_end(&mut self) {
        self.cursor = self.line_bounds().1;
    }

    /// One line up, keeping the column where possible. `false` on the
    /// first line, so the caller can treat Up as history recall.
    pub fn move_up(&mut self) -> bool {
        let (start, _) = self.line_bounds();
        if start == 0 {
            return false;
        }
        let column = self.cursor - start;
        self.cursor = start - 1;
        let (previous_start, previous_end) = self.line_bounds();
        self.cursor = previous_start + column.min(previous_end - previous_start);
        true
    }

    /// One line down, keeping the column where possible. `false` on the
    /// last line.
    pub fn move_down(&mut self) -> bool {
        let (start, end) = self.line_bounds();
        if end >= self.grapheme_count() {
            return false;
        }
        let column = self.cursor - start;
        self.cursor = end + 1;
        let (next_start, next_end) = self.line_bounds();
        self.cursor = next_start + column.min(next_end - next_start);
        true
    }

    /// Grapheme range `[start, end)` of the caret's logical line (end is
    /// the index of its `\n`, or the buffer end).
    fn line_bounds(&self) -> (usize, usize) {
        let graphemes: Vec<&str> = self.text.graphemes(true).collect();
        let cursor = self.cursor.min(graphemes.len());
        let start = graphemes[..cursor]
            .iter()
            .rposition(|g| *g == "\n")
            .map_or(0, |at| at + 1);
        let end = graphemes[cursor..]
            .iter()
            .position(|g| *g == "\n")
            .map_or(graphemes.len(), |at| cursor + at);
        (start, end)
    }

    /// Start of the word that ends at or before `from`, skipping the
    /// spaces in between.
    fn word_start_before(&self, from: usize) -> usize {
        let graphemes: Vec<&str> = self.text.graphemes(true).collect();
        let mut at = from.min(graphemes.len());
        while at > 0 && is_space(graphemes[at - 1]) {
            at -= 1;
        }
        while at > 0 && !is_space(graphemes[at - 1]) {
            at -= 1;
        }
        at
    }

    fn remove_graphemes(&mut self, from: usize, to: usize) {
        if from >= to {
            return;
        }
        let start = self.byte_offset(from);
        let end = self.byte_offset(to);
        self.text.replace_range(start..end, "");
    }

    /// Total wrapped rows at this width (minimum 1) — the panel uses it
    /// to size the input area before clamping to a maximum.
    pub fn rows(&self, width: usize) -> usize {
        self.wrap_rows(width).len().max(1)
    }

    /// Render the editor as panel lines: prompt prefix on the first
    /// row, aligned continuations, the caret drawn as a reverse cell,
    /// windowed to `max_rows` ending at the caret row.
    pub fn render(&self, width: usize, max_rows: usize) -> Vec<Line> {
        if self.text.is_empty() {
            return vec![Line {
                spans: vec![
                    Span::styled(self.prefix(0), self.prompt_style),
                    caret_span(" "),
                    Span::styled(self.placeholder.clone(), self.placeholder_style),
                ],
            }];
        }

        let rows = self.wrap_rows(width);
        let (caret_row, caret_col) = self.caret_position(&rows);
        let max_rows = max_rows.max(1);
        let first = caret_row.saturating_sub(max_rows - 1);

        rows.iter()
            .enumerate()
            .skip(first)
            .take(max_rows)
            .map(|(i, row)| {
                let mut spans = vec![Span::styled(self.prefix(i), self.prompt_style)];
                if i == caret_row {
                    let before: String = row.graphemes[..caret_col].concat();
                    if !before.is_empty() {
                        spans.push(Span::raw(before));
                    }
                    let under = row.graphemes.get(caret_col).map(|g| g.as_str());
                    spans.push(caret_span(under.unwrap_or(" ")));
                    if caret_col < row.graphemes.len() {
                        let after: String = row.graphemes[caret_col + 1..].concat();
                        if !after.is_empty() {
                            spans.push(Span::raw(after));
                        }
                    }
                } else if !row.graphemes.is_empty() {
                    spans.push(Span::raw(row.graphemes.concat()));
                }
                Line { spans }
            })
            .collect()
    }

    fn prefix(&self, row: usize) -> String {
        if row == 0 {
            format!(" {}", self.prompt)
        } else {
            " ".repeat(self.prefix_width())
        }
    }

    fn prefix_width(&self) -> usize {
        1 + visible_width(&self.prompt)
    }

    fn grapheme_count(&self) -> usize {
        self.text.graphemes(true).count()
    }

    fn byte_offset(&self, grapheme_index: usize) -> usize {
        self.text
            .grapheme_indices(true)
            .nth(grapheme_index)
            .map(|(at, _)| at)
            .unwrap_or(self.text.len())
    }

    /// Hard-wrap the buffer into rows of at most the content width.
    fn wrap_rows(&self, width: usize) -> Vec<WrappedRow> {
        let text_width = width.saturating_sub(self.prefix_width() + 1).max(1);
        let mut rows = Vec::new();
        let mut index = 0usize;
        for logical in self.text.split('\n') {
            let mut row = WrappedRow::new(index);
            let mut row_width = 0usize;
            for grapheme in logical.graphemes(true) {
                let w = visible_width(grapheme);
                if row_width + w > text_width && !row.graphemes.is_empty() {
                    rows.push(std::mem::replace(&mut row, WrappedRow::new(index)));
                    row_width = 0;
                }
                row.graphemes.push(grapheme.to_string());
                row_width += w;
                index += 1;
            }
            rows.push(row);
            index += 1; // the '\n' consumes one grapheme index
        }
        rows
    }

    /// Locate the caret on the wrapped grid: prefer the row that starts
    /// exactly at the caret (wrap point / after newline), else the row
    /// containing it, else the end of the last row.
    fn caret_position(&self, rows: &[WrappedRow]) -> (usize, usize) {
        for (i, row) in rows.iter().enumerate() {
            if row.start == self.cursor && (i + 1 < rows.len() || row.graphemes.is_empty()) {
                return (i, 0);
            }
            let end = row.start + row.graphemes.len();
            if self.cursor >= row.start && self.cursor < end {
                return (i, self.cursor - row.start);
            }
        }
        match rows.last() {
            Some(last) => (
                rows.len() - 1,
                (self.cursor.saturating_sub(last.start)).min(last.graphemes.len()),
            ),
            None => (0, 0),
        }
    }
}

struct WrappedRow {
    /// Grapheme index of the row's first cell.
    start: usize,
    graphemes: Vec<String>,
}

impl WrappedRow {
    fn new(start: usize) -> Self {
        WrappedRow {
            start,
            graphemes: Vec::new(),
        }
    }
}

/// Word separators for word motions: whitespace, newlines included.
fn is_space(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

fn caret_span(under: &str) -> Span {
    Span::styled(
        under,
        Style {
            reverse: true,
            ..Style::default()
        },
    )
}

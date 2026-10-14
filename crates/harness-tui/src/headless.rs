//! In-memory terminals for tests: `TestTerminal` applies row updates and
//! reads back text, styles, and the drawn caret; `VtScreen` interprets
//! the raw bytes a `core::Screen` writes, scrollback included.

use crate::diff::RowUpdate;
use crate::text::{Color, Line, Style, visible_width};

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

/// A byte-level VT screen for end-to-end layout tests: feed it exactly
/// the bytes `core::Screen` writes, then read back the viewport and the
/// native scrollback. Understands the subset the library emits — CUP,
/// ED 0/2/3, EL 0/2, CR, LF (LF on the last row scrolls the top row
/// into scrollback) and pending-wrap autowrap at the right margin — plus
/// the alternate screen (private mode 1049: the main viewport and cursor
/// are saved and restored, and the alternate screen has no scrollback).
/// SGR, other private modes and OSC strings are skipped. Wide characters
/// take two cells; zero-width marks join the previous cell.
pub struct VtScreen {
    width: usize,
    height: usize,
    /// Viewport cells; a blank cell is " ", the right half of a wide
    /// character is "".
    rows: Vec<Vec<String>>,
    row: usize,
    col: usize,
    pending_wrap: bool,
    scrollback: Vec<String>,
    /// The main viewport and cursor while the alternate screen is shown.
    saved_main: Option<(Vec<Vec<String>>, usize, usize)>,
}

impl VtScreen {
    pub fn new(width: u16, height: u16) -> Self {
        let width = (width as usize).max(1);
        let height = (height as usize).max(1);
        VtScreen {
            width,
            height,
            rows: vec![vec![" ".to_string(); width]; height],
            row: 0,
            col: 0,
            pending_wrap: false,
            scrollback: Vec::new(),
            saved_main: None,
        }
    }

    /// Interpret a chunk of terminal output.
    pub fn feed(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            match ch {
                '\x1b' => match chars.next() {
                    Some('[') => {
                        let mut params = String::new();
                        let mut final_byte = None;
                        for next in chars.by_ref() {
                            if ('\x40'..='\x7e').contains(&next) {
                                final_byte = Some(next);
                                break;
                            }
                            params.push(next);
                        }
                        if let Some(final_byte) = final_byte {
                            self.csi(&params, final_byte);
                        }
                    }
                    Some(']') => {
                        // OSC: skip to BEL or ST (ESC \).
                        while let Some(next) = chars.next() {
                            if next == '\x07' {
                                break;
                            }
                            if next == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                                break;
                            }
                        }
                    }
                    _ => {}
                },
                '\r' => {
                    self.col = 0;
                    self.pending_wrap = false;
                }
                '\n' => {
                    self.line_feed();
                    self.pending_wrap = false;
                }
                ch if ch.is_control() => {}
                ch => self.print(ch),
            }
        }
    }

    /// Visible rows, trailing blanks trimmed.
    pub fn viewport(&self) -> Vec<String> {
        self.rows.iter().map(|cells| row_text(cells)).collect()
    }

    /// Rows scrolled off the top, oldest first.
    pub fn scrollback(&self) -> &[String] {
        &self.scrollback
    }

    /// Scrollback followed by the viewport — everything a user can see
    /// by scrolling, top to bottom.
    pub fn history(&self) -> Vec<String> {
        let mut all = self.scrollback.clone();
        all.extend(self.viewport());
        all
    }

    /// 0-based (row, col) of the cursor.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    /// True while the alternate screen is shown.
    pub fn in_alt_screen(&self) -> bool {
        self.saved_main.is_some()
    }

    fn csi(&mut self, params: &str, final_byte: char) {
        if let Some(modes) = params.strip_prefix('?') {
            if modes.split(';').any(|mode| mode == "1049") {
                match final_byte {
                    'h' if self.saved_main.is_none() => {
                        let blank = vec![vec![" ".to_string(); self.width]; self.height];
                        let main = std::mem::replace(&mut self.rows, blank);
                        self.saved_main = Some((main, self.row, self.col));
                    }
                    'l' => {
                        if let Some((main, row, col)) = self.saved_main.take() {
                            self.rows = main;
                            self.row = row;
                            self.col = col;
                        }
                    }
                    _ => {}
                }
                self.pending_wrap = false;
            }
            return;
        }
        let nums: Vec<usize> = params
            .split(';')
            .map(|part| part.parse().unwrap_or(0))
            .collect();
        let first = nums.first().copied().unwrap_or(0);
        match final_byte {
            'H' | 'f' => {
                self.row = first.max(1).min(self.height) - 1;
                let col = nums.get(1).copied().unwrap_or(1);
                self.col = col.max(1).min(self.width) - 1;
                self.pending_wrap = false;
            }
            'J' => match first {
                0 => {
                    self.clear_cells(self.row, self.col..self.width);
                    for row in self.row + 1..self.height {
                        self.clear_cells(row, 0..self.width);
                    }
                }
                2 => {
                    for row in 0..self.height {
                        self.clear_cells(row, 0..self.width);
                    }
                }
                3 => self.scrollback.clear(),
                _ => {}
            },
            'K' => match first {
                0 => self.clear_cells(self.row, self.col..self.width),
                2 => self.clear_cells(self.row, 0..self.width),
                _ => {}
            },
            _ => {}
        }
    }

    fn clear_cells(&mut self, row: usize, cols: std::ops::Range<usize>) {
        for cell in &mut self.rows[row][cols] {
            *cell = " ".to_string();
        }
    }

    fn line_feed(&mut self) {
        if self.row + 1 == self.height {
            let top = self.rows.remove(0);
            if self.saved_main.is_none() {
                self.scrollback.push(row_text(&top));
            }
            self.rows.push(vec![" ".to_string(); self.width]);
        } else {
            self.row += 1;
        }
    }

    fn print(&mut self, ch: char) {
        let mut buf = [0u8; 4];
        let width = visible_width(ch.encode_utf8(&mut buf));
        if width == 0 {
            // Combining mark: join the cell just written.
            let col = self.col.saturating_sub(usize::from(!self.pending_wrap));
            self.rows[self.row][col].push(ch);
            return;
        }
        if self.pending_wrap || self.col + width > self.width {
            self.col = 0;
            self.line_feed();
            self.pending_wrap = false;
        }
        self.rows[self.row][self.col] = ch.to_string();
        if width == 2 && self.col + 1 < self.width {
            self.rows[self.row][self.col + 1] = String::new();
        }
        if self.col + width >= self.width {
            self.col = self.width - 1;
            self.pending_wrap = true;
        } else {
            self.col += width;
        }
    }
}

fn row_text(cells: &[String]) -> String {
    cells.concat().trim_end().to_string()
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

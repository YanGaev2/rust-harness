//! Screen: a pinned bottom panel plus native-scrollback content.
//!
//! The Claude-Code-style screen model: finished content is printed into
//! the terminal's own scrollback (wheel-scrollable, selectable, survives
//! exit) and only the bottom panel (editor, spinner, status) is diffed
//! and repainted. Every write happens inside a synchronized-output frame.

use std::io;

use crate::diff::diff_frames;
use crate::terminal::{Terminal, TerminalError, esc};
use crate::text::{Line, render_ansi};

/// A live screen: pinned panel at the bottom, scrollback above.
pub struct Screen {
    terminal: Terminal,
    width: u16,
    height: u16,
    /// Currently painted panel rows.
    panel: Vec<Line>,
    /// Terminal row (0-based) where the panel starts — also where the
    /// next scrollback emission begins.
    origin: u16,
}

impl Screen {
    /// Wrap an already-open terminal. `start_row` is where content may
    /// begin (the cursor row at startup); tests pass it explicitly.
    pub fn new(terminal: Terminal, width: u16, height: u16, start_row: u16) -> Screen {
        Screen {
            terminal,
            width,
            height,
            panel: Vec::new(),
            origin: start_row.min(height.saturating_sub(1)),
        }
    }

    /// Attach to the real terminal: queries size and cursor position so
    /// the panel starts where the shell prompt left off.
    pub fn stdout() -> Result<Screen, TerminalError> {
        let terminal = Terminal::stdout()?;
        let (width, height) = crate::terminal::size()?;
        let (row, _col) = crate::terminal::cursor_position()?;
        Ok(Screen::new(terminal, width, height, row))
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Rows available for the pinned panel.
    pub fn max_panel_rows(&self) -> u16 {
        self.height
    }

    /// Print finished content into native scrollback, then repaint the
    /// panel right below it. Lines must already be wrapped to `width`.
    pub fn emit(&mut self, lines: &[Line]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let panel_len = self.panel.len() as u16;
        let row0 = self.origin;
        let k = lines.len() as u16;
        let height = self.height.max(1);

        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(row0, 0));
        frame.push_str(esc::CLEAR_DOWN);
        for line in lines {
            frame.push_str(&render_ansi(line));
            frame.push_str("\r\n");
        }
        // Printing scrolled the buffer when it ran past the last row;
        // scroll further if the panel reserve still doesn't fit.
        let scrolled = (row0 + k).saturating_sub(height - 1);
        let needed = (row0 + k + panel_len).saturating_sub(height);
        let extra = needed.saturating_sub(scrolled);
        if extra > 0 {
            frame.push_str(&esc::move_to(height - 1, 0));
            for _ in 0..extra {
                frame.push_str("\r\n");
            }
        }
        // Reserve at least the cursor row even with no panel painted,
        // so origin can never land outside the viewport.
        self.origin = (row0 + k).min(height.saturating_sub(panel_len.max(1)));
        push_panel_rows(&mut frame, &self.panel, self.origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Repaint the pinned panel. Same height → only changed rows are
    /// rewritten; height changes force a clear + full redraw.
    pub fn render_panel(&mut self, mut lines: Vec<Line>) -> io::Result<()> {
        // A panel taller than the screen is tail-clipped: the bottom
        // rows (editor, status) matter most, and unclipped input would
        // underflow the origin arithmetic below.
        let max_rows = self.height.max(1) as usize;
        if lines.len() > max_rows {
            lines = lines.split_off(lines.len() - max_rows);
        }
        if lines.len() == self.panel.len() {
            let updates = diff_frames(&self.panel, &lines);
            self.panel = lines;
            return self.terminal.present(&updates, self.origin);
        }
        let height = self.height.max(1);
        let new_len = lines.len() as u16;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        // Scroll content up when the taller panel no longer fits below.
        let overflow = (self.origin + new_len).saturating_sub(height);
        if overflow > 0 {
            frame.push_str(&esc::move_to(height - 1, 0));
            for _ in 0..overflow {
                frame.push_str("\r\n");
            }
            self.origin = self.origin.saturating_sub(overflow);
        }
        frame.push_str(&esc::move_to(self.origin, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &lines, self.origin);
        frame.push_str(esc::SYNC_END);
        self.panel = lines;
        self.terminal.write_all(frame.as_bytes())
    }

    /// Adopt a new terminal size: pin the panel to the bottom and fully
    /// redraw it (the terminal reflowed the content above on its own).
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        let panel_len = self.panel.len() as u16;
        self.origin = self.height.saturating_sub(panel_len.max(1));
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(self.origin, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &self.panel, self.origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible screen and the terminal's scrollback and forget
    /// the painted panel — the `/new`-style full reset. The next
    /// `render_panel`/`emit` starts from the top row.
    pub fn clear(&mut self) -> io::Result<()> {
        self.panel.clear();
        self.origin = 0;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(esc::CLEAR_SCROLLBACK);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Clear the panel area and leave the cursor below the scrollback —
    /// call before exiting so the shell prompt lands cleanly.
    pub fn release(&mut self) -> io::Result<()> {
        let mut frame = String::new();
        frame.push_str(&esc::move_to(self.origin, 0));
        frame.push_str(esc::CLEAR_DOWN);
        self.terminal.write_all(frame.as_bytes())
    }
}

fn push_panel_rows(frame: &mut String, panel: &[Line], origin: u16) {
    for (i, line) in panel.iter().enumerate() {
        frame.push_str(&esc::move_to(origin + i as u16, 0));
        frame.push_str(esc::CLEAR_LINE);
        frame.push_str(&render_ansi(line));
    }
}

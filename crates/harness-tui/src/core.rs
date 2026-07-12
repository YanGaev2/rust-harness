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

/// A live screen: panel pinned to the bottom rows, content flowing
/// top-down above it into native scrollback.
pub struct Screen {
    terminal: Terminal,
    width: u16,
    height: u16,
    /// Currently painted panel rows (always at the bottom of the screen).
    panel: Vec<Line>,
    /// Terminal row (0-based) where the next scrollback emission begins —
    /// the end of the content printed so far.
    cursor: u16,
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
            cursor: start_row.min(height.saturating_sub(1)),
        }
    }

    /// Bottom row where the current panel starts.
    fn panel_row(&self) -> u16 {
        self.height
            .max(1)
            .saturating_sub(self.panel.len().max(1) as u16)
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

    /// Print finished content into native scrollback below the previous
    /// content, then repaint the panel pinned at the bottom. Lines must
    /// already be wrapped to `width`.
    pub fn emit(&mut self, lines: &[Line]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let panel_len = self.panel.len() as u16;
        let row0 = self.cursor;
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
        // so the content cursor can never land outside the viewport.
        self.cursor = (row0 + k).min(height.saturating_sub(panel_len.max(1)));
        push_panel_rows(&mut frame, &self.panel, self.panel_row());
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Repaint the bottom-pinned panel. Same height → only changed rows
    /// are rewritten; height changes force a clear + full redraw, and a
    /// taller panel scrolls content up to make room.
    pub fn render_panel(&mut self, mut lines: Vec<Line>) -> io::Result<()> {
        // A panel taller than the screen is tail-clipped: the bottom
        // rows (editor, status) matter most, and unclipped input would
        // underflow the pinning arithmetic below.
        let height = self.height.max(1);
        let max_rows = height as usize;
        if lines.len() > max_rows {
            lines = lines.split_off(lines.len() - max_rows);
        }
        let target = height - lines.len() as u16;
        if lines.len() == self.panel.len() {
            let updates = diff_frames(&self.panel, &lines);
            self.panel = lines;
            return self.terminal.present(&updates, target);
        }
        let old_target = height.saturating_sub(self.panel.len() as u16);
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        // Scroll content up when the taller panel would overlap it.
        let overflow = self.cursor.saturating_sub(target);
        if overflow > 0 {
            frame.push_str(&esc::move_to(height - 1, 0));
            for _ in 0..overflow {
                frame.push_str("\r\n");
            }
            self.cursor -= overflow;
        }
        // Clear from wherever panel rows may be stale: the old panel top
        // when the panel shrank, the new top when it grew.
        frame.push_str(&esc::move_to(target.min(old_target), 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &lines, target);
        frame.push_str(esc::SYNC_END);
        self.panel = lines;
        self.terminal.write_all(frame.as_bytes())
    }

    /// Adopt a new terminal size: keep the panel pinned to the bottom
    /// and fully redraw it (the terminal reflowed the content above on
    /// its own).
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        let max_rows = self.height as usize;
        if self.panel.len() > max_rows {
            let clipped = self.panel.split_off(self.panel.len() - max_rows);
            self.panel = clipped;
        }
        let target = self.panel_row();
        self.cursor = self.cursor.min(target);
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(target, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &self.panel, target);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Claim the whole window at startup: scroll whatever the shell left
    /// on screen into native scrollback (still reachable by scrolling up,
    /// unlike `clear`) and start with a blank viewport — content will
    /// begin at the top row, the panel pins to the bottom.
    pub fn takeover(&mut self) -> io::Result<()> {
        let height = self.height.max(1);
        self.panel.clear();
        self.cursor = 0;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(height - 1, 0));
        for _ in 0..height {
            frame.push_str("\r\n");
        }
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible screen and the terminal's scrollback and forget
    /// the painted panel — the `/new`-style full reset. The next
    /// emission starts from the top row and the next panel paint pins
    /// back to the bottom.
    pub fn clear(&mut self) -> io::Result<()> {
        self.panel.clear();
        self.cursor = 0;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(esc::CLEAR_SCROLLBACK);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Clear everything below the content (the gap and the panel) and
    /// leave the cursor right after the scrollback — call before exiting
    /// so the shell prompt lands cleanly under the transcript.
    pub fn release(&mut self) -> io::Result<()> {
        let mut frame = String::new();
        frame.push_str(&esc::move_to(self.cursor, 0));
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

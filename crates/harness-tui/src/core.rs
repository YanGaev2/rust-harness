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
    /// Opt-in bottom-pinned layout: the panel hugs the last rows of the
    /// viewport and every repaint/resize re-derives its top from the
    /// bottom edge instead of following the content flow.
    anchored: bool,
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
            anchored: false,
        }
    }

    /// Switch to the bottom-pinned layout (Claude-Code-style chat): the
    /// flow origin moves to the last viewport row so committed content
    /// accumulates just above the panel, and repaints/resizes keep the
    /// panel glued to the bottom edge. Call before the first paint (or
    /// right before `clear_screen`) — an already-painted panel is not
    /// relocated retroactively.
    pub fn set_bottom_anchor(&mut self, anchored: bool) {
        self.anchored = anchored;
        if anchored && self.panel.is_empty() {
            self.origin = self.height.saturating_sub(1);
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
        // so origin can never land outside the viewport. Anchored mode
        // ignores the flow and pins the panel to the bottom edge.
        self.origin = if self.anchored {
            height.saturating_sub(panel_len.max(1))
        } else {
            (row0 + k).min(height.saturating_sub(panel_len.max(1)))
        };
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
        if self.anchored {
            let target = height.saturating_sub(new_len.max(1));
            if target > self.origin {
                // Panel shrank: wipe the old panel area first, then
                // repin to the bottom (the flow origin would leave the
                // shorter panel floating with a gap below it).
                frame.push_str(&esc::move_to(self.origin, 0));
                frame.push_str(esc::CLEAR_DOWN);
                self.origin = target;
            }
        }
        frame.push_str(&esc::move_to(self.origin, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &lines, self.origin);
        frame.push_str(esc::SYNC_END);
        self.panel = lines;
        self.terminal.write_all(frame.as_bytes())
    }

    /// Commit finalized rows at the current flow origin and paint the
    /// next live frame directly below them — in ONE synchronized write,
    /// so the terminal never shows the stale live frame between the two.
    /// The scroll reserve is computed from the NEW live frame, not the
    /// previously painted one.
    pub fn present(&mut self, committed: &[Line], mut live: Vec<Line>) -> io::Result<()> {
        let height = self.height.max(1);
        let max_rows = height as usize;
        if live.len() > max_rows {
            live = live.split_off(live.len() - max_rows);
        }
        // Fast path: nothing to commit and the live frame kept its
        // height — diff rows in place.
        if committed.is_empty() && live.len() == self.panel.len() {
            let updates = diff_frames(&self.panel, &live);
            self.panel = live;
            return self.terminal.present(&updates, self.origin);
        }
        let row0 = self.origin;
        let k = committed.len() as u16;
        let live_len = live.len() as u16;

        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(row0, 0));
        frame.push_str(esc::CLEAR_DOWN);
        for line in committed {
            frame.push_str(&render_ansi(line));
            frame.push_str("\r\n");
        }
        // Printing scrolled the buffer when it ran past the last row;
        // scroll further only if the NEW live frame still doesn't fit.
        let scrolled = (row0 + k).saturating_sub(height - 1);
        let needed = (row0 + k + live_len).saturating_sub(height);
        let extra = needed.saturating_sub(scrolled);
        if extra > 0 {
            frame.push_str(&esc::move_to(height - 1, 0));
            for _ in 0..extra {
                frame.push_str("\r\n");
            }
        }
        let origin = if self.anchored {
            height.saturating_sub(live_len.max(1))
        } else {
            (row0 + k).min(height.saturating_sub(live_len.max(1)))
        };
        push_panel_rows(&mut frame, &live, origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())?;
        self.origin = origin;
        self.panel = live;
        Ok(())
    }

    /// Adopt a new terminal size: keep the content-following flow origin
    /// (clamped so the panel still fits) and fully redraw the live frame
    /// there. The origin is never derived from the bottom edge — resize
    /// must not reinstate a bottom-pinned layout.
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        let max_rows = self.height as usize;
        if self.panel.len() > max_rows {
            let clipped = self.panel.split_off(self.panel.len() - max_rows);
            self.panel = clipped;
        }
        let panel_len = self.panel.len() as u16;
        let clamp = self.height.saturating_sub(panel_len.max(1));
        // Anchored mode repins to the new bottom edge; flow mode keeps
        // the content-following origin (clamped so the panel fits).
        let clear_from = if self.anchored {
            let target = clamp;
            let clear_from = self.origin.min(target);
            self.origin = target;
            clear_from
        } else {
            self.origin = self.origin.min(clamp);
            self.origin
        };
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(clear_from, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &self.panel, self.origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Adopt a new size after a width change and wipe the viewport.
    /// Width changes make the terminal reflow its buffer, so every row
    /// coordinate recorded before the resize — including the painted
    /// panel's — is stale; erasing the whole viewport is the only
    /// cleanup that needs no coordinates. The painted panel is
    /// forgotten; follow up with [`Screen::repaint`] once the size
    /// settles. Native scrollback is never touched.
    pub fn resize_erase(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        self.panel.clear();
        self.origin = self.reset_origin();
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Repaint the whole viewport from the model: the content tail sits
    /// directly above the panel (anchored mode) or flows from the top
    /// with the panel below it (flow mode). Absolute row writes only —
    /// nothing scrolls, so nothing is duplicated into scrollback.
    pub fn repaint(&mut self, content: &[Line], mut live: Vec<Line>) -> io::Result<()> {
        let height = self.height.max(1);
        let max_rows = height as usize;
        if live.len() > max_rows {
            live = live.split_off(live.len() - max_rows);
        }
        let live_len = live.len() as u16;
        let origin = if self.anchored {
            height.saturating_sub(live_len.max(1))
        } else {
            (content.len() as u16).min(height.saturating_sub(live_len.max(1)))
        };
        // Content fills the rows above the panel, tail-clipped to fit.
        let space = origin as usize;
        let tail_start = content.len().saturating_sub(space);
        let tail = &content[tail_start..];
        let first_row = origin - tail.len() as u16;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        for (i, line) in tail.iter().enumerate() {
            frame.push_str(&esc::move_to(first_row + i as u16, 0));
            frame.push_str(&render_ansi(line));
        }
        push_panel_rows(&mut frame, &live, origin);
        frame.push_str(esc::SYNC_END);
        self.origin = origin;
        self.panel = live;
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible viewport and home the cursor — the startup
    /// claim. A deliberate product choice built on pi's `clearScreen`
    /// primitive (pi itself starts at the shell cursor; we clear so the
    /// TUI owns the window from row 0). The user's existing terminal
    /// scrollback is left untouched.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.panel.clear();
        self.origin = self.reset_origin();
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible screen and the terminal's scrollback and forget
    /// the painted panel — the `/new`-style full reset. The next
    /// `render_panel`/`emit` starts from the top row.
    pub fn clear(&mut self) -> io::Result<()> {
        self.panel.clear();
        self.origin = self.reset_origin();
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

impl Screen {
    /// Where a wiped screen starts painting: the top row in flow mode,
    /// the last row in anchored mode (so the first frame lands at the
    /// bottom instead of the top of the blank viewport).
    fn reset_origin(&self) -> u16 {
        if self.anchored {
            self.height.saturating_sub(1)
        } else {
            0
        }
    }
}

fn push_panel_rows(frame: &mut String, panel: &[Line], origin: u16) {
    for (i, line) in panel.iter().enumerate() {
        frame.push_str(&esc::move_to(origin + i as u16, 0));
        frame.push_str(esc::CLEAR_LINE);
        frame.push_str(&render_ansi(line));
    }
}

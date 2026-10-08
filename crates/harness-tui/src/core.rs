//! Screen: native-scrollback content plus a live panel below it.
//!
//! Finished content is printed into the terminal's own scrollback
//! (wheel-scrollable, selectable, survives exit) and only the live panel
//! (streaming tail, editor, spinner, status) is diffed and repainted.
//! Every write happens inside a synchronized-output frame.
//!
//! Layout contract: content always flows top-down and every commit
//! starts exactly where the previous content ended, so the scrollback
//! never receives blank gaps. The live panel follows the content. Its
//! last `docked` rows (editor, status) are pinned to the bottom edge:
//! blank filler rows between the panel body and the dock absorb every
//! change of panel height. The filler belongs to the panel, so it is
//! repainted, never committed.

use std::io;

use crate::diff::diff_frames;
use crate::terminal::{Terminal, TerminalError, esc};
use crate::text::{Line, render_ansi};

/// A live screen: scrollback content above, the live panel below it.
pub struct Screen {
    terminal: Terminal,
    width: u16,
    height: u16,
    /// Currently painted panel rows, dock filler included.
    panel: Vec<Line>,
    /// The live frame as last passed in (no filler) — resize recomposes
    /// it against the new geometry.
    live: Vec<Line>,
    /// How many trailing `live` rows are docked to the bottom edge.
    docked: usize,
    /// Terminal row (0-based) where the panel starts — also where the
    /// next committed content begins.
    origin: u16,
    /// The terminal's background color, when it reported one.
    background: Option<(u8, u8, u8)>,
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
            live: Vec::new(),
            docked: 0,
            origin: start_row.min(height.saturating_sub(1)),
            background: None,
        }
    }

    /// Attach to the real terminal: queries size, cursor position (so the
    /// panel starts where the shell prompt left off) and background color
    /// (OSC 11, else `COLORFGBG`).
    pub fn stdout() -> Result<Screen, TerminalError> {
        let terminal = Terminal::stdout()?;
        let (width, height) = crate::terminal::size()?;
        let probe = crate::terminal::startup_probe()?;
        let mut screen = Screen::new(terminal, width, height, probe.cursor.0);
        screen.background = probe
            .background
            .or_else(crate::terminal::colorfgbg_background);
        Ok(screen)
    }

    /// The terminal's background color, when it reported one — pick the
    /// light or dark palette from it.
    pub fn background(&self) -> Option<(u8, u8, u8)> {
        self.background
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Rows available for the live panel.
    pub fn max_panel_rows(&self) -> u16 {
        self.height
    }

    /// Print finished content into native scrollback, then repaint the
    /// current live frame right below it. Lines must already be wrapped
    /// to `width`.
    pub fn emit(&mut self, lines: &[Line]) -> io::Result<()> {
        if lines.is_empty() {
            return Ok(());
        }
        let live = self.live.clone();
        self.present(lines, live, self.docked)
    }

    /// Repaint the live panel in flow layout (no docked rows). Same
    /// height → only changed rows are rewritten.
    pub fn render_panel(&mut self, lines: Vec<Line>) -> io::Result<()> {
        self.present(&[], lines, 0)
    }

    /// Commit finalized rows at the current origin and paint the next
    /// live frame directly below them — in ONE synchronized write, so the
    /// terminal never shows a stale live frame between the two. The last
    /// `docked` rows of `live` are pinned to the bottom edge. The scroll
    /// reserve comes from the NEW live frame without filler: filler only
    /// fills rows that are free anyway, it never pushes content up.
    pub fn present(
        &mut self,
        committed: &[Line],
        mut live: Vec<Line>,
        docked: usize,
    ) -> io::Result<()> {
        let height = self.height.max(1);
        // A frame taller than the screen is tail-clipped: the bottom rows
        // (editor, status) matter most, and an unclipped frame would
        // underflow the origin arithmetic below.
        if live.len() > height as usize {
            live = live.split_off(live.len() - height as usize);
        }
        let docked = docked.min(live.len());
        let row0 = self.origin;
        let k = committed.len() as u16;
        let live_len = live.len() as u16;
        let origin = (row0 + k).min(height.saturating_sub(live_len.max(1)));
        let panel = compose(&live, docked, (height - origin) as usize);

        // Fast path: nothing to commit, the panel stays put and keeps its
        // height — diff rows in place.
        if committed.is_empty() && origin == row0 && panel.len() == self.panel.len() {
            let updates = diff_frames(&self.panel, &panel);
            self.panel = panel;
            self.live = live;
            self.docked = docked;
            return self.terminal.present(&updates, origin);
        }

        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(row0, 0));
        frame.push_str(esc::CLEAR_DOWN);
        for line in committed {
            frame.push_str(&render_ansi(line));
            frame.push_str("\r\n");
        }
        // Printing scrolled the buffer when it ran past the last row;
        // scroll further only if the new live frame still doesn't fit.
        let scrolled = (row0 + k).saturating_sub(height - 1);
        let needed = (row0 + k + live_len).saturating_sub(height);
        let extra = needed.saturating_sub(scrolled);
        if extra > 0 {
            frame.push_str(&esc::move_to(height - 1, 0));
            for _ in 0..extra {
                frame.push_str("\r\n");
            }
        }
        push_panel_rows(&mut frame, &panel, origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())?;
        self.origin = origin;
        self.panel = panel;
        self.live = live;
        self.docked = docked;
        Ok(())
    }

    /// Adopt a new terminal size: keep the content-following origin
    /// (clamped so the live frame still fits), recompose the dock filler
    /// for the new height and redraw the panel there.
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        let max_rows = self.height as usize;
        if self.live.len() > max_rows {
            self.live = self.live.split_off(self.live.len() - max_rows);
        }
        self.docked = self.docked.min(self.live.len());
        let live_len = self.live.len() as u16;
        self.origin = self.origin.min(self.height.saturating_sub(live_len.max(1)));
        self.panel = compose(
            &self.live,
            self.docked,
            (self.height - self.origin) as usize,
        );
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(&esc::move_to(self.origin, 0));
        frame.push_str(esc::CLEAR_DOWN);
        push_panel_rows(&mut frame, &self.panel, self.origin);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Adopt a new size and wipe the viewport. A width change makes the
    /// terminal reflow its buffer, and a height change may scroll it, so
    /// every row coordinate recorded before the resize — including the
    /// painted panel's — is stale; erasing the whole viewport is the only
    /// cleanup that needs no coordinates. The painted panel is
    /// forgotten; follow up with [`Screen::repaint`] once the size
    /// settles. Native scrollback is never touched.
    pub fn resize_erase(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        self.panel.clear();
        self.origin = 0;
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Repaint the whole viewport from the model: the content tail flows
    /// from the top, the live panel follows it, docked rows sit on the
    /// bottom edge. Absolute row writes only — nothing scrolls, so
    /// nothing is duplicated into scrollback.
    pub fn repaint(
        &mut self,
        content: &[Line],
        mut live: Vec<Line>,
        docked: usize,
    ) -> io::Result<()> {
        let height = self.height.max(1);
        if live.len() > height as usize {
            live = live.split_off(live.len() - height as usize);
        }
        let docked = docked.min(live.len());
        let live_len = live.len() as u16;
        let origin = (content.len() as u16).min(height.saturating_sub(live_len.max(1)));
        // Content fills the rows above the panel, tail-clipped to fit.
        let tail = &content[content.len().saturating_sub(origin as usize)..];
        let first_row = origin - tail.len() as u16;
        let panel = compose(&live, docked, (height - origin) as usize);
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        for (i, line) in tail.iter().enumerate() {
            frame.push_str(&esc::move_to(first_row + i as u16, 0));
            frame.push_str(&render_ansi(line));
        }
        push_panel_rows(&mut frame, &panel, origin);
        frame.push_str(esc::SYNC_END);
        self.origin = origin;
        self.panel = panel;
        self.live = live;
        self.docked = docked;
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible viewport and home the cursor — the startup
    /// claim. A deliberate product choice built on pi's `clearScreen`
    /// primitive (pi itself starts at the shell cursor; we clear so the
    /// TUI owns the window from row 0). The user's existing terminal
    /// scrollback is left untouched.
    pub fn clear_screen(&mut self) -> io::Result<()> {
        self.forget();
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(&esc::move_to(0, 0));
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Wipe the visible screen and the terminal's scrollback and forget
    /// the painted panel — the `/new`-style full reset. The next frame
    /// starts from the top row.
    pub fn clear(&mut self) -> io::Result<()> {
        self.forget();
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

    fn forget(&mut self) {
        self.panel.clear();
        self.live.clear();
        self.docked = 0;
        self.origin = 0;
    }
}

/// The painted panel for `rows` free rows below the origin: the live
/// body, then blank filler, then the docked rows on the bottom edge.
/// Without docked rows (or without spare room) the frame is the live
/// rows as-is.
fn compose(live: &[Line], docked: usize, rows: usize) -> Vec<Line> {
    if docked == 0 || live.len() >= rows {
        return live.to_vec();
    }
    let split = live.len() - docked;
    let mut panel = Vec::with_capacity(rows);
    panel.extend_from_slice(&live[..split]);
    panel.resize(rows - docked, Line::default());
    panel.extend_from_slice(&live[split..]);
    panel
}

fn push_panel_rows(frame: &mut String, panel: &[Line], origin: u16) {
    for (i, line) in panel.iter().enumerate() {
        frame.push_str(&esc::move_to(origin + i as u16, 0));
        frame.push_str(esc::CLEAR_LINE);
        frame.push_str(&render_ansi(line));
    }
}

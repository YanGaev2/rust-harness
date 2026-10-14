//! Full-screen mode on the alternate screen buffer.
//!
//! The application owns every row. The transcript scrolls inside the
//! app, the editor and status rows stay on the bottom edge, and the
//! mouse wheel and selection come in as input events. Each frame is
//! diffed row by row against the painted one; a resize repaints all
//! rows, because the alternate screen has no scrollback to reflow. On
//! [`FullScreen::leave`] the terminal shows the main buffer again, as it
//! was before the session, and [`FullScreen::print`] can write the
//! conversation into its native scrollback.

use std::io;

use crate::diff::diff_frames;
use crate::terminal::{Terminal, TerminalError, esc};
use crate::text::{Line, render_ansi};

pub struct FullScreen {
    terminal: Terminal,
    width: u16,
    height: u16,
    /// Rows on screen now; empty forces a full repaint.
    painted: Vec<Line>,
    /// On the alternate screen (between construction and `leave`).
    active: bool,
    background: Option<(u8, u8, u8)>,
    foreground: Option<(u8, u8, u8)>,
}

impl FullScreen {
    /// Attach to the real terminal: query size and colors (OSC 10/11,
    /// `COLORFGBG` as a fallback), then switch to the alternate screen
    /// and capture the mouse.
    pub fn stdout() -> Result<FullScreen, TerminalError> {
        let terminal = Terminal::stdout()?;
        let (width, height) = crate::terminal::size()?;
        let probe = crate::terminal::startup_probe()?;
        let mut screen = FullScreen::new(terminal, width, height)?;
        screen.background = probe
            .background
            .or_else(crate::terminal::colorfgbg_background);
        screen.foreground = probe.foreground;
        crate::terminal::mark_alt_screen(true);
        Ok(screen)
    }

    /// Enter full-screen mode on an open terminal; tests pass a backend.
    pub fn new(mut terminal: Terminal, width: u16, height: u16) -> io::Result<FullScreen> {
        terminal.set_alt_screen(true);
        let mut setup = String::new();
        setup.push_str(esc::ALT_SCREEN_ON);
        setup.push_str(esc::MOUSE_ON);
        setup.push_str(esc::CLEAR_ALL);
        terminal.write_all(setup.as_bytes())?;
        Ok(FullScreen {
            terminal,
            width,
            height: height.max(1),
            painted: Vec::new(),
            active: true,
            background: None,
            foreground: None,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// The terminal's background color, when it reported one.
    pub fn background(&self) -> Option<(u8, u8, u8)> {
        self.background
    }

    /// The terminal's default text color, when it reported one.
    pub fn foreground(&self) -> Option<(u8, u8, u8)> {
        self.foreground
    }

    /// Paint a whole frame. Missing rows are blank, extra rows are cut;
    /// only rows that differ from the painted frame are written, in one
    /// synchronized update.
    pub fn draw(&mut self, mut frame: Vec<Line>) -> io::Result<()> {
        frame.resize(self.height as usize, Line::default());
        let updates = diff_frames(&self.painted, &frame);
        self.terminal.present(&updates, 0)?;
        self.painted = frame;
        Ok(())
    }

    /// Adopt a new size: clear the screen now, repaint every row on the
    /// next `draw`.
    pub fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        self.width = width;
        self.height = height.max(1);
        self.painted.clear();
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        frame.push_str(esc::CLEAR_ALL);
        frame.push_str(esc::SYNC_END);
        self.terminal.write_all(frame.as_bytes())
    }

    /// Put `text` on the system clipboard (OSC 52).
    pub fn copy(&mut self, text: &str) -> io::Result<()> {
        self.terminal
            .write_all(esc::copy_to_clipboard(text).as_bytes())
    }

    /// Release the mouse and return to the main buffer, as it was before
    /// the session. Safe to call twice.
    pub fn leave(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        self.active = false;
        self.terminal.set_alt_screen(false);
        crate::terminal::mark_alt_screen(false);
        let mut frame = String::new();
        frame.push_str(esc::SYNC_END);
        frame.push_str(esc::MOUSE_OFF);
        frame.push_str(esc::ALT_SCREEN_OFF);
        self.terminal.write_all(frame.as_bytes())
    }

    /// After [`FullScreen::leave`]: print lines at the cursor of the main
    /// buffer, where they become native scrollback.
    pub fn print(&mut self, lines: &[Line]) -> io::Result<()> {
        let mut out = String::new();
        for line in lines {
            out.push_str(&render_ansi(line));
            out.push_str("\r\n");
        }
        self.terminal.write_all(out.as_bytes())
    }
}

impl Drop for FullScreen {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

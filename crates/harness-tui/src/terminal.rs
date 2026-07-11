//! Platform layer: escape sequences, raw mode, and guaranteed restore.

use std::fmt;
use std::io;

/// Errors from the terminal layer. Hand-rolled per repo convention.
#[derive(Debug)]
pub enum TerminalError {
    NotATty,
    Io(io::Error),
    Platform(&'static str),
}

impl fmt::Display for TerminalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TerminalError::NotATty => write!(f, "stdin/stdout is not a terminal"),
            TerminalError::Io(err) => write!(f, "terminal io error: {err}"),
            TerminalError::Platform(call) => {
                write!(f, "terminal platform call failed: {call}")
            }
        }
    }
}

impl std::error::Error for TerminalError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TerminalError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for TerminalError {
    fn from(err: io::Error) -> Self {
        TerminalError::Io(err)
    }
}

/// VT escape sequences. `move_to` takes 0-based coordinates (the escape
/// itself is 1-based).
pub mod esc {
    pub const HIDE_CURSOR: &str = "\x1b[?25l";
    pub const SHOW_CURSOR: &str = "\x1b[?25h";
    pub const SYNC_BEGIN: &str = "\x1b[?2026h";
    pub const SYNC_END: &str = "\x1b[?2026l";
    pub const BRACKETED_PASTE_ON: &str = "\x1b[?2004h";
    pub const BRACKETED_PASTE_OFF: &str = "\x1b[?2004l";
    /// SGR mouse capture (1000 = button tracking, 1006 = SGR encoding).
    /// Not enabled by `Terminal` setup: capturing the mouse breaks
    /// native-scrollback wheel scrolling. The input layer toggles it.
    pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1006h";
    pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1000l";
    pub const CLEAR_LINE: &str = "\x1b[2K";

    pub fn move_to(row: u16, col: u16) -> String {
        format!("\x1b[{};{}H", row + 1, col + 1)
    }
}

/// Everything a session must write to leave the terminal usable, in
/// safe order: close any open synchronized frame, release the mouse,
/// turn bracketed paste off, show the cursor. Raw-mode restore is
/// separate (console modes, not escapes).
pub fn restore_sequence() -> &'static str {
    "\x1b[?2026l\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?25h"
}

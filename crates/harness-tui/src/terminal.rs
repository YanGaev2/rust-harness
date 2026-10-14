//! Platform layer: escape sequences, raw mode, and guaranteed restore.

use std::fmt;
use std::io::{self, Write};
use std::sync::Once;

use crate::diff::RowUpdate;
use crate::text::render_ansi;

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

/// Callers that carry a plain `io::Error` (both harness front ends do) fold
/// the terminal error into it instead of hand-wrapping the message.
impl From<TerminalError> for io::Error {
    fn from(err: TerminalError) -> Self {
        match err {
            TerminalError::Io(err) => err,
            other => io::Error::other(other.to_string()),
        }
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
    /// Push / pop kitty keyboard protocol flag 1 ("disambiguate escape
    /// codes"): Shift+Enter, Ctrl/Alt chords and Esc become unambiguous
    /// `CSI … u` keys. Terminals without the protocol ignore both.
    pub const KITTY_KEYS_ON: &str = "\x1b[>1u";
    pub const KITTY_KEYS_OFF: &str = "\x1b[<u";
    /// SGR mouse capture: 1000 = press/release, 1002 = motion while a
    /// button is held (drag selection), 1006 = SGR encoding. Not enabled
    /// by `Terminal` setup: capturing the mouse breaks native-scrollback
    /// wheel scrolling. `fullscreen::FullScreen` turns it on.
    pub const MOUSE_ON: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
    pub const MOUSE_OFF: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";
    /// Enter / leave the alternate screen buffer (xterm 1049: the main
    /// buffer and its cursor are saved, then restored on leave).
    pub const ALT_SCREEN_ON: &str = "\x1b[?1049h";
    pub const ALT_SCREEN_OFF: &str = "\x1b[?1049l";
    pub const CLEAR_LINE: &str = "\x1b[2K";
    /// Erase from the cursor to the end of the screen.
    pub const CLEAR_DOWN: &str = "\x1b[0J";
    /// Erase the whole visible screen.
    pub const CLEAR_ALL: &str = "\x1b[2J";
    /// Erase the terminal's scrollback buffer (xterm extension).
    pub const CLEAR_SCROLLBACK: &str = "\x1b[3J";

    pub fn move_to(row: u16, col: u16) -> String {
        format!("\x1b[{};{}H", row + 1, col + 1)
    }

    /// OSC 52: put `text` on the system clipboard through the terminal
    /// (Windows Terminal, kitty, WezTerm, iTerm2, tmux with
    /// `set-clipboard on`). Works over SSH too.
    pub fn copy_to_clipboard(text: &str) -> String {
        format!("\x1b]52;c;{}\x07", super::base64(text.as_bytes()))
    }
}

/// Standard base64 with padding — enough for OSC 52.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = match *chunk {
            [a, b, c] => (a as u32) << 16 | (b as u32) << 8 | c as u32,
            [a, b] => (a as u32) << 16 | (b as u32) << 8,
            [a] => (a as u32) << 16,
            _ => unreachable!("chunks(3) yields 1 to 3 bytes"),
        };
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Everything a session must write to leave the terminal usable, in
/// safe order: close any open synchronized frame, release the mouse,
/// pop the kitty keyboard flags, turn bracketed paste off, show the
/// cursor. Raw-mode restore is separate (console modes, not escapes).
/// Leaving the alternate screen is separate too ([`Terminal`] and the
/// panic hook add it only when it was entered: on the main screen,
/// xterm's 1049 reset would also restore a stale saved cursor).
pub fn restore_sequence() -> &'static str {
    "\x1b[?2026l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[<u\x1b[?2004l\x1b[?25h"
}

/// Set while a real terminal shows the alternate screen, so a panic
/// can leave it before the panic message prints.
static ALT_SCREEN_ACTIVE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Tell the panic hook whether the real terminal is on the alternate
/// screen (`fullscreen::FullScreen::stdout` and `leave` call this).
pub fn mark_alt_screen(active: bool) {
    ALT_SCREEN_ACTIVE.store(active, std::sync::atomic::Ordering::SeqCst);
}

/// True only when both stdin and stdout are terminals — the TUI needs
/// raw key input *and* a screen to draw on.
pub fn is_tty() -> bool {
    sys::is_tty()
}

/// Whether the terminal renders 24-bit SGR colors. Windows consoles do
/// (conhost since Windows 10 1703, Windows Terminal always); elsewhere
/// trust `COLORTERM`, plus `WT_SESSION` for WSL inside Windows Terminal.
pub fn supports_truecolor() -> bool {
    if cfg!(windows) || std::env::var_os("WT_SESSION").is_some() {
        return true;
    }
    std::env::var("COLORTERM")
        .map(|value| matches!(value.as_str(), "truecolor" | "24bit"))
        .unwrap_or(false)
}

/// (width, height) of the stdout terminal in cells.
pub fn size() -> Result<(u16, u16), TerminalError> {
    sys::size()
}

/// Blocking read of raw input bytes from the terminal. Returns the
/// number of bytes read (0 = EOF). Runs on the event-pump thread.
pub fn read_input(buf: &mut [u8]) -> io::Result<usize> {
    sys::read_input(buf)
}

/// Raw input mode + bracketed paste WITHOUT the full TUI setup — the
/// cursor stays visible. For line-mode front ends that print their own
/// output. Everything restores on drop.
pub struct RawModeHandle {
    _raw: sys::RawModeGuard,
}

impl Drop for RawModeHandle {
    fn drop(&mut self) {
        let mut out = io::stdout();
        let _ = out.write_all(esc::BRACKETED_PASTE_OFF.as_bytes());
        let _ = out.flush();
    }
}

pub fn raw_mode() -> Result<RawModeHandle, TerminalError> {
    sys::enable_vt()?;
    let raw = sys::RawModeGuard::enable()?;
    let mut out = io::stdout();
    out.write_all(esc::BRACKETED_PASTE_ON.as_bytes())?;
    out.flush()?;
    Ok(RawModeHandle { _raw: raw })
}

/// What the startup probe learned about the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartupProbe {
    /// 0-based (row, col) of the cursor.
    pub cursor: (u16, u16),
    /// Default background color, when the terminal answers OSC 11.
    pub background: Option<(u8, u8, u8)>,
    /// Default text color, when the terminal answers OSC 10.
    pub foreground: Option<(u8, u8, u8)>,
}

/// Ask the terminal where the cursor is (DSR) and for its text and
/// background colors (OSC 10, OSC 11) in one round trip. Terminals answer
/// in order and every VT terminal answers DSR, so an OSC reply that has
/// not arrived by the cursor report is not coming — no timeout is needed.
/// Call only at startup, before the input pump owns stdin: the replies
/// arrive interleaved with any pending keystrokes.
pub fn startup_probe() -> Result<StartupProbe, TerminalError> {
    let mut out = io::stdout();
    out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[6n")?;
    out.flush()?;
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 64];
    for _ in 0..32 {
        let n = sys::read_input(&mut chunk)?;
        if n == 0 {
            break;
        }
        collected.extend_from_slice(&chunk[..n]);
        // Keep reading until a real `ESC [ row ; col R` parses: a pending
        // keystroke may contain an `R` or an `ESC [` of its own.
        if let Some(cursor) = parse_cursor_report(&collected) {
            return Ok(StartupProbe {
                cursor,
                background: parse_background_report(&collected),
                foreground: parse_foreground_report(&collected),
            });
        }
    }
    Err(TerminalError::Platform("cursor position report"))
}

/// Find `ESC [ row ; col R` in a byte stream that may contain unrelated
/// pending input around it.
pub fn parse_cursor_report(bytes: &[u8]) -> Option<(u16, u16)> {
    (0..bytes.len()).find_map(|start| {
        let rest = bytes.get(start..)?.strip_prefix(b"\x1b[")?;
        let end = rest
            .iter()
            .position(|&b| !(b.is_ascii_digit() || b == b';'))?;
        if rest[end] != b'R' {
            return None;
        }
        let body = std::str::from_utf8(&rest[..end]).ok()?;
        let (row, col) = body.split_once(';')?;
        Some((
            row.parse::<u16>().ok()?.saturating_sub(1),
            col.parse::<u16>().ok()?.saturating_sub(1),
        ))
    })
}

/// Parse an OSC 11 reply — `ESC ] 11 ; rgb:RRRR/GGGG/BBBB` ended by BEL or
/// ST — into 8-bit channels (1 to 4 hex digits per channel are allowed).
pub fn parse_background_report(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    parse_color_report(bytes, b"\x1b]11;")
}

/// Parse an OSC 10 (default text color) reply; same format as OSC 11.
pub fn parse_foreground_report(bytes: &[u8]) -> Option<(u8, u8, u8)> {
    parse_color_report(bytes, b"\x1b]10;")
}

fn parse_color_report(bytes: &[u8], introducer: &[u8]) -> Option<(u8, u8, u8)> {
    let start = bytes
        .windows(introducer.len())
        .position(|w| w == introducer)?;
    let rest = &bytes[start + introducer.len()..];
    let end = rest.iter().position(|&b| b == 0x07 || b == 0x1b)?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let mut channels = body.strip_prefix("rgb:")?.split('/').map(|hex| {
        let digits = hex.len().clamp(1, 4) as u32;
        let value = u32::from_str_radix(hex, 16).ok()?;
        let max = 16u32.pow(digits) - 1;
        Some((value * 255 / max) as u8)
    });
    Some((channels.next()??, channels.next()??, channels.next()??))
}

/// Background hint from the `COLORFGBG` convention (`fg;bg`, set by
/// rxvt, Konsole and others): palette slots 7 and 9–15 are light.
pub fn colorfgbg_background() -> Option<(u8, u8, u8)> {
    let value = std::env::var("COLORFGBG").ok()?;
    let slot: u8 = value.rsplit(';').next()?.parse().ok()?;
    Some(if slot == 7 || (9..=15).contains(&slot) {
        (255, 255, 255)
    } else {
        (0, 0, 0)
    })
}

/// The live terminal: owns the output writer, keeps raw mode for its
/// lifetime, restores everything on drop.
pub struct Terminal {
    out: Box<dyn Write + Send>,
    _raw: Option<sys::RawModeGuard>,
    /// The alternate screen is on: drop must leave it.
    alt_screen: bool,
}

impl Terminal {
    /// Attach to the real terminal. Errors with `NotATty` when stdio is
    /// piped — callers fall back to line mode.
    pub fn stdout() -> Result<Terminal, TerminalError> {
        if !sys::is_tty() {
            return Err(TerminalError::NotATty);
        }
        sys::enable_vt()?;
        let raw = sys::RawModeGuard::enable()?;
        let mut terminal = Terminal {
            out: Box::new(io::stdout()),
            _raw: Some(raw),
            alt_screen: false,
        };
        terminal.write_setup()?;
        Ok(terminal)
    }

    /// Same escape behavior against an injected writer; no tty or raw
    /// mode. This is how tests observe exact bytes.
    pub fn with_backend(out: Box<dyn Write + Send>) -> Terminal {
        let mut terminal = Terminal {
            out,
            _raw: None,
            alt_screen: false,
        };
        let _ = terminal.write_setup();
        terminal
    }

    /// Record that the alternate screen is on (or off again), so drop
    /// leaves it before the shell gets the terminal back.
    pub fn set_alt_screen(&mut self, active: bool) {
        self.alt_screen = active;
    }

    /// Write raw bytes and flush — the primitive `core::Screen` builds
    /// its synchronized frames on.
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.out.write_all(bytes)?;
        self.out.flush()
    }

    fn write_setup(&mut self) -> Result<(), TerminalError> {
        self.out.write_all(esc::HIDE_CURSOR.as_bytes())?;
        self.out.write_all(esc::BRACKETED_PASTE_ON.as_bytes())?;
        self.out.write_all(esc::KITTY_KEYS_ON.as_bytes())?;
        self.out.flush()?;
        Ok(())
    }

    /// Write one frame of row updates atomically: the whole batch is
    /// wrapped in synchronized-output markers so the terminal applies
    /// it without intermediate states. `origin_row` is the terminal row
    /// (0-based) where the pinned panel starts; update rows are
    /// relative to it.
    pub fn present(&mut self, updates: &[RowUpdate], origin_row: u16) -> io::Result<()> {
        if updates.is_empty() {
            return Ok(());
        }
        let mut frame = String::new();
        frame.push_str(esc::SYNC_BEGIN);
        for update in updates {
            match update {
                RowUpdate::Write { row, line } => {
                    frame.push_str(&esc::move_to(origin_row + *row as u16, 0));
                    frame.push_str(esc::CLEAR_LINE);
                    frame.push_str(&render_ansi(line));
                }
                RowUpdate::Clear { row } => {
                    frame.push_str(&esc::move_to(origin_row + *row as u16, 0));
                    frame.push_str(esc::CLEAR_LINE);
                }
            }
        }
        frame.push_str(esc::SYNC_END);
        self.out.write_all(frame.as_bytes())?;
        self.out.flush()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.alt_screen {
            let _ = self.out.write_all(esc::SYNC_END.as_bytes());
            let _ = self.out.write_all(esc::ALT_SCREEN_OFF.as_bytes());
        }
        let _ = self.out.write_all(restore_sequence().as_bytes());
        let _ = self.out.flush();
        // `_raw` drops after this, restoring console modes.
    }
}

static PANIC_RESTORE: Once = Once::new();

/// Install a panic hook that restores the terminal before the default
/// hook prints the panic. Idempotent; chains the previous hook.
pub fn install_panic_restore() {
    PANIC_RESTORE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_now();
            previous(info);
        }));
    });
}

/// Best-effort immediate restore: escapes to stderr (stdout may be the
/// panicking writer) plus console-mode restore. Safe to call anytime.
pub fn restore_now() {
    let mut err = io::stderr();
    if ALT_SCREEN_ACTIVE.swap(false, std::sync::atomic::Ordering::SeqCst) {
        let _ = err.write_all(esc::SYNC_END.as_bytes());
        let _ = err.write_all(esc::ALT_SCREEN_OFF.as_bytes());
    }
    let _ = err.write_all(restore_sequence().as_bytes());
    let _ = err.flush();
    sys::restore_console();
}

#[cfg(all(unix, not(target_os = "linux")))]
compile_error!(
    "harness-tui supports Windows and Linux (WSL2) only: the unix FFI layer \
     hard-codes the linux-gnu termios layout and ioctl numbers"
);

#[cfg(unix)]
mod sys {
    use super::TerminalError;
    use std::sync::Mutex;

    // linux-gnu (glibc) layout; our supported Linux is WSL2.
    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(dead_code)]
    struct Termios {
        c_iflag: u32,
        c_oflag: u32,
        c_cflag: u32,
        c_lflag: u32,
        c_line: u8,
        c_cc: [u8; 32],
        c_ispeed: u32,
        c_ospeed: u32,
    }

    #[repr(C)]
    struct WinSize {
        row: u16,
        col: u16,
        xpixel: u16,
        ypixel: u16,
    }

    unsafe extern "C" {
        fn isatty(fd: i32) -> i32;
        fn tcgetattr(fd: i32, termios: *mut Termios) -> i32;
        fn tcsetattr(fd: i32, optional_actions: i32, termios: *const Termios) -> i32;
        fn ioctl(fd: i32, request: u64, ...) -> i32;
    }

    const STDIN_FD: i32 = 0;
    const STDOUT_FD: i32 = 1;
    const TCSANOW: i32 = 0;
    const TIOCGWINSZ: u64 = 0x5413;
    const ISIG: u32 = 0o1;
    const ICANON: u32 = 0o2;
    const ECHO: u32 = 0o10;
    const IEXTEN: u32 = 0o100000;
    const ICRNL: u32 = 0o400;
    const IXON: u32 = 0o2000;
    /// linux-gnu `c_cc` indices.
    const VTIME_INDEX: usize = 5;
    const VMIN_INDEX: usize = 6;

    static SAVED: Mutex<Option<Termios>> = Mutex::new(None);

    pub fn is_tty() -> bool {
        unsafe { isatty(STDIN_FD) == 1 && isatty(STDOUT_FD) == 1 }
    }

    /// Unix terminals speak VT natively; nothing to enable.
    pub fn enable_vt() -> Result<(), TerminalError> {
        Ok(())
    }

    pub fn size() -> Result<(u16, u16), TerminalError> {
        let mut ws = WinSize {
            row: 0,
            col: 0,
            xpixel: 0,
            ypixel: 0,
        };
        let rc = unsafe { ioctl(STDOUT_FD, TIOCGWINSZ, &mut ws as *mut WinSize) };
        if rc != 0 || ws.col == 0 {
            return Err(TerminalError::Platform("ioctl(TIOCGWINSZ)"));
        }
        Ok((ws.col, ws.row))
    }

    /// Raw input mode; restores the saved termios on drop. OPOST stays
    /// on so `\n` keeps working for scrollback printing.
    pub struct RawModeGuard {
        _private: (),
    }

    impl RawModeGuard {
        pub fn enable() -> Result<Self, TerminalError> {
            let mut original: Termios = unsafe { std::mem::zeroed() };
            if unsafe { tcgetattr(STDIN_FD, &mut original) } != 0 {
                return Err(TerminalError::Platform("tcgetattr"));
            }
            *SAVED.lock().unwrap() = Some(original);
            let mut raw = original;
            raw.c_lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
            raw.c_iflag &= !(IXON | ICRNL);
            // Inherited VMIN=0 would turn reads into polling EOFs and
            // VMIN>1 would block single keys — pin byte-at-a-time reads.
            raw.c_cc[VTIME_INDEX] = 0;
            raw.c_cc[VMIN_INDEX] = 1;
            if unsafe { tcsetattr(STDIN_FD, TCSANOW, &raw) } != 0 {
                return Err(TerminalError::Platform("tcsetattr"));
            }
            Ok(RawModeGuard { _private: () })
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            restore_console();
        }
    }

    /// Idempotent: restores the saved termios once, then becomes a no-op.
    pub fn restore_console() {
        if let Ok(mut saved) = SAVED.lock()
            && let Some(original) = saved.take()
        {
            unsafe {
                tcsetattr(STDIN_FD, TCSANOW, &original);
            }
        }
    }

    pub fn read_input(buf: &mut [u8]) -> std::io::Result<usize> {
        unsafe extern "C" {
            fn read(fd: i32, buf: *mut core::ffi::c_void, count: usize) -> isize;
        }
        let n = unsafe { read(STDIN_FD, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

#[cfg(windows)]
mod sys {
    use super::TerminalError;
    use std::sync::atomic::{AtomicU32, Ordering};

    type Handle = *mut core::ffi::c_void;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[allow(dead_code)]
    struct ConsoleScreenBufferInfo {
        size: Coord,
        cursor_position: Coord,
        attributes: u16,
        window: SmallRect,
        maximum_window_size: Coord,
    }

    unsafe extern "system" {
        fn GetStdHandle(std_handle: u32) -> Handle;
        fn GetConsoleMode(handle: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(handle: Handle, mode: u32) -> i32;
        fn GetConsoleScreenBufferInfo(handle: Handle, info: *mut ConsoleScreenBufferInfo) -> i32;
        fn GetConsoleCP() -> u32;
        fn SetConsoleCP(code_page: u32) -> i32;
    }

    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    const ENABLE_PROCESSED_OUTPUT: u32 = 0x0001;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const DISABLE_NEWLINE_AUTO_RETURN: u32 = 0x0008;
    const CP_UTF8: u32 = 65001;

    /// u32::MAX = "nothing saved" sentinel.
    static SAVED_IN: AtomicU32 = AtomicU32::new(u32::MAX);
    static SAVED_OUT: AtomicU32 = AtomicU32::new(u32::MAX);
    static SAVED_CP: AtomicU32 = AtomicU32::new(u32::MAX);

    pub fn is_tty() -> bool {
        let mut mode = 0u32;
        let stdin_ok = unsafe { GetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), &mut mode) } != 0;
        let stdout_ok = unsafe { GetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), &mut mode) } != 0;
        stdin_ok && stdout_ok
    }

    /// Turn on VT escape processing for stdout (Windows 10+).
    pub fn enable_vt() -> Result<(), TerminalError> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let mut mode = 0u32;
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(TerminalError::Platform("GetConsoleMode(stdout)"));
        }
        SAVED_OUT.store(mode, Ordering::SeqCst);
        // DISABLE_NEWLINE_AUTO_RETURN keeps an exact-width bottom row
        // from scrolling the console the moment its last column is
        // written (which would silently shift the pinned panel).
        let desired = mode
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            | DISABLE_NEWLINE_AUTO_RETURN;
        if unsafe { SetConsoleMode(handle, desired) } == 0 {
            // Older conhost may reject DISABLE_NEWLINE_AUTO_RETURN.
            if unsafe { SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) } == 0 {
                return Err(TerminalError::Platform("SetConsoleMode(stdout, VT)"));
            }
        }
        Ok(())
    }

    pub fn size() -> Result<(u16, u16), TerminalError> {
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        let mut info: ConsoleScreenBufferInfo = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(handle, &mut info) } == 0 {
            return Err(TerminalError::Platform("GetConsoleScreenBufferInfo"));
        }
        let width = (info.window.right - info.window.left + 1) as u16;
        let height = (info.window.bottom - info.window.top + 1) as u16;
        Ok((width, height))
    }

    /// Raw input mode; restores saved console modes on drop.
    pub struct RawModeGuard {
        _private: (),
    }

    impl RawModeGuard {
        pub fn enable() -> Result<Self, TerminalError> {
            let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
            let mut mode = 0u32;
            if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
                return Err(TerminalError::Platform("GetConsoleMode(stdin)"));
            }
            SAVED_IN.store(mode, Ordering::SeqCst);
            let raw = (mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT))
                | ENABLE_VIRTUAL_TERMINAL_INPUT;
            if unsafe { SetConsoleMode(handle, raw) } == 0 {
                return Err(TerminalError::Platform("SetConsoleMode(stdin, raw)"));
            }
            // ReadFile decodes keys via the console INPUT code page,
            // which is often an OEM page — force UTF-8 so non-ASCII
            // input (e.g. Cyrillic) survives the byte parser.
            SAVED_CP.store(unsafe { GetConsoleCP() }, Ordering::SeqCst);
            unsafe {
                SetConsoleCP(CP_UTF8);
            }
            Ok(RawModeGuard { _private: () })
        }
    }

    impl Drop for RawModeGuard {
        fn drop(&mut self) {
            restore_console();
        }
    }

    /// Idempotent: swaps the sentinel back in, so a second call is a no-op.
    pub fn restore_console() {
        let saved_in = SAVED_IN.swap(u32::MAX, Ordering::SeqCst);
        if saved_in != u32::MAX {
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), saved_in);
            }
        }
        let saved_out = SAVED_OUT.swap(u32::MAX, Ordering::SeqCst);
        if saved_out != u32::MAX {
            unsafe {
                SetConsoleMode(GetStdHandle(STD_OUTPUT_HANDLE), saved_out);
            }
        }
        let saved_cp = SAVED_CP.swap(u32::MAX, Ordering::SeqCst);
        if saved_cp != u32::MAX {
            unsafe {
                SetConsoleCP(saved_cp);
            }
        }
    }

    /// With `ENABLE_VIRTUAL_TERMINAL_INPUT` set, `ReadFile` on the
    /// console input handle delivers keys as a VT byte stream.
    pub fn read_input(buf: &mut [u8]) -> std::io::Result<usize> {
        unsafe extern "system" {
            fn ReadFile(
                handle: super::sys::Handle,
                buffer: *mut core::ffi::c_void,
                to_read: u32,
                read: *mut u32,
                overlapped: *mut core::ffi::c_void,
            ) -> i32;
        }
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut read = 0u32;
        let ok = unsafe {
            ReadFile(
                handle,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(read as usize)
        }
    }
}

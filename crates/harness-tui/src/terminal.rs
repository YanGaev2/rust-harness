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
    /// Erase from the cursor to the end of the screen.
    pub const CLEAR_DOWN: &str = "\x1b[0J";
    /// Erase the whole visible screen.
    pub const CLEAR_ALL: &str = "\x1b[2J";
    /// Erase the terminal's scrollback buffer (xterm extension).
    pub const CLEAR_SCROLLBACK: &str = "\x1b[3J";

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

/// True only when both stdin and stdout are terminals — the TUI needs
/// raw key input *and* a screen to draw on.
pub fn is_tty() -> bool {
    sys::is_tty()
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

/// Ask the terminal where the cursor is (DSR): returns 0-based
/// (row, col). Call only at startup, before the input pump owns stdin —
/// the response arrives interleaved with any pending keystrokes.
pub fn cursor_position() -> Result<(u16, u16), TerminalError> {
    let mut out = io::stdout();
    out.write_all(b"\x1b[6n")?;
    out.flush()?;
    let mut collected: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 32];
    for _ in 0..16 {
        let n = sys::read_input(&mut chunk)?;
        if n == 0 {
            break;
        }
        collected.extend_from_slice(&chunk[..n]);
        if collected.contains(&b'R') {
            break;
        }
    }
    parse_cursor_report(&collected).ok_or(TerminalError::Platform("cursor position report"))
}

/// Extract `ESC [ row ; col R` from a byte stream that may contain
/// unrelated pending input around it.
fn parse_cursor_report(bytes: &[u8]) -> Option<(u16, u16)> {
    let start = bytes.windows(2).position(|w| w == b"\x1b[")?;
    let rest = &bytes[start + 2..];
    let end = rest.iter().position(|&b| b == b'R')?;
    let body = std::str::from_utf8(&rest[..end]).ok()?;
    let (row, col) = body.split_once(';')?;
    Some((
        row.parse::<u16>().ok()?.saturating_sub(1),
        col.parse::<u16>().ok()?.saturating_sub(1),
    ))
}

/// The live terminal: owns the output writer, keeps raw mode for its
/// lifetime, restores everything on drop.
pub struct Terminal {
    out: Box<dyn Write + Send>,
    _raw: Option<sys::RawModeGuard>,
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
        };
        terminal.write_setup()?;
        Ok(terminal)
    }

    /// Same escape behavior against an injected writer; no tty or raw
    /// mode. This is how tests observe exact bytes.
    pub fn with_backend(out: Box<dyn Write + Send>) -> Terminal {
        let mut terminal = Terminal { out, _raw: None };
        let _ = terminal.write_setup();
        terminal
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

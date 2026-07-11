use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use harness_tui::diff::RowUpdate;
use harness_tui::terminal::{Terminal, TerminalError, esc, restore_sequence};
use harness_tui::text::Line;

/// Shared byte sink so the test can read what Terminal wrote after
/// handing ownership of the writer to it.
#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn contents(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn move_to_converts_zero_based_to_one_based() {
    assert_eq!(esc::move_to(0, 0), "\x1b[1;1H");
    assert_eq!(esc::move_to(4, 9), "\x1b[5;10H");
}

#[test]
fn cursor_and_sync_escapes_are_standard() {
    assert_eq!(esc::HIDE_CURSOR, "\x1b[?25l");
    assert_eq!(esc::SHOW_CURSOR, "\x1b[?25h");
    assert_eq!(esc::SYNC_BEGIN, "\x1b[?2026h");
    assert_eq!(esc::SYNC_END, "\x1b[?2026l");
    assert_eq!(esc::BRACKETED_PASTE_ON, "\x1b[?2004h");
    assert_eq!(esc::BRACKETED_PASTE_OFF, "\x1b[?2004l");
    assert_eq!(esc::MOUSE_ON, "\x1b[?1000h\x1b[?1006h");
    assert_eq!(esc::MOUSE_OFF, "\x1b[?1006l\x1b[?1000l");
    assert_eq!(esc::CLEAR_LINE, "\x1b[2K");
}

#[test]
fn restore_sequence_undoes_every_mode_we_set() {
    let seq = restore_sequence();
    assert!(seq.contains(esc::SYNC_END));
    assert!(seq.contains(esc::MOUSE_OFF));
    assert!(seq.contains(esc::BRACKETED_PASTE_OFF));
    assert!(seq.contains(esc::SHOW_CURSOR));
}

// Platform queries must be safe to call in any environment. Under the
// test harness stdio may or may not be a real console, so assert
// consistency, not a fixed answer.
#[test]
fn size_is_consistent_with_tty_state() {
    let tty = harness_tui::terminal::is_tty();
    let size = harness_tui::terminal::size();
    if tty {
        let (width, height) = size.expect("a tty must report its size");
        assert!(width > 0 && height > 0);
    }
    // Not a tty: size may fail — the important part is it returns an
    // error instead of panicking, which reaching this line proves.
}

#[test]
fn terminal_error_displays_human_messages() {
    assert_eq!(
        TerminalError::NotATty.to_string(),
        "stdin/stdout is not a terminal"
    );
    assert_eq!(
        TerminalError::Platform("tcgetattr").to_string(),
        "terminal platform call failed: tcgetattr"
    );
}

#[test]
fn stdout_errors_when_not_a_tty() {
    // Only assert in a piped environment (CI); on a real console the
    // constructor legitimately succeeds.
    if harness_tui::terminal::is_tty() {
        return;
    }
    match Terminal::stdout() {
        Err(TerminalError::NotATty) => {}
        Err(other) => panic!("expected NotATty, got: {other}"),
        Ok(_) => panic!("expected NotATty error off a terminal"),
    }
}

#[test]
fn construction_hides_cursor_and_enables_bracketed_paste() {
    let buf = SharedBuf::default();
    let _term = Terminal::with_backend(Box::new(buf.clone()));
    let out = buf.contents();
    assert!(out.contains(esc::HIDE_CURSOR));
    assert!(out.contains(esc::BRACKETED_PASTE_ON));
}

#[test]
fn present_wraps_updates_in_synchronized_frame() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let updates = vec![RowUpdate::Write {
        row: 1,
        line: Line::raw("hi"),
    }];
    term.present(&updates, 10).unwrap();
    let out = buf.contents();
    let frame_start = out.find(esc::SYNC_BEGIN).expect("sync begin");
    let frame_end = out.find(esc::SYNC_END).expect("sync end");
    assert!(frame_start < frame_end);
    // origin 10 + row 1 → escape row 12 (1-based), column 1.
    assert!(out.contains("\x1b[12;1H"));
    assert!(out.contains(esc::CLEAR_LINE));
    assert!(out.contains("hi"));
}

#[test]
fn present_clear_row_writes_no_text() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let before = buf.contents();
    term.present(&[RowUpdate::Clear { row: 0 }], 5).unwrap();
    let frame = buf.contents()[before.len()..].to_string();
    assert!(frame.contains("\x1b[6;1H"));
    assert!(frame.contains(esc::CLEAR_LINE));
}

#[test]
fn present_with_no_updates_writes_nothing() {
    let buf = SharedBuf::default();
    let mut term = Terminal::with_backend(Box::new(buf.clone()));
    let before = buf.contents();
    term.present(&[], 0).unwrap();
    assert_eq!(buf.contents(), before);
}

#[test]
fn drop_writes_restore_sequence() {
    let buf = SharedBuf::default();
    {
        let _term = Terminal::with_backend(Box::new(buf.clone()));
    }
    let out = buf.contents();
    assert!(out.contains(esc::SHOW_CURSOR));
    assert!(out.contains(esc::BRACKETED_PASTE_OFF));
}

#[test]
fn install_panic_restore_is_idempotent() {
    harness_tui::terminal::install_panic_restore();
    harness_tui::terminal::install_panic_restore();
}

#[test]
fn restore_now_is_safe_anytime() {
    harness_tui::terminal::restore_now();
    harness_tui::terminal::restore_now();
}

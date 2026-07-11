use harness_tui::terminal::{TerminalError, esc, restore_sequence};

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

use harness_tui::diff::{RowUpdate, diff_frames};
use harness_tui::headless::TestTerminal;
use harness_tui::text::{Color, Line, Span, Style};

#[test]
fn new_terminal_is_empty() {
    let term = TestTerminal::new(80);
    assert_eq!(term.width(), 80);
    assert_eq!(term.text(), "");
}

#[test]
fn apply_writes_rows_in_order() {
    let mut term = TestTerminal::new(80);
    let next = vec![Line::raw("one"), Line::raw("two")];
    term.apply(&diff_frames(&[], &next));
    assert_eq!(term.text(), "one\ntwo");
}

#[test]
fn apply_write_beyond_end_pads_with_empty_rows() {
    let mut term = TestTerminal::new(80);
    term.apply(&[RowUpdate::Write {
        row: 2,
        line: Line::raw("x"),
    }]);
    assert_eq!(term.text(), "\n\nx");
}

#[test]
fn apply_clear_blanks_the_row() {
    let mut term = TestTerminal::new(80);
    let next = vec![Line::raw("one"), Line::raw("two")];
    term.apply(&diff_frames(&[], &next));
    term.apply(&[RowUpdate::Clear { row: 0 }]);
    assert_eq!(term.text(), "\ntwo");
}

#[test]
fn styled_snapshot_shows_caret_as_reverse_cell() {
    let mut term = TestTerminal::new(80);
    let caret = Style {
        reverse: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("hi "), Span::styled(" ", caret)],
    };
    term.apply(&[RowUpdate::Write { row: 0, line }]);
    assert_eq!(term.styled(), "hi [ ]{reverse}");
}

#[test]
fn styled_snapshot_lists_attributes_and_colors() {
    let mut term = TestTerminal::new(80);
    let style = Style {
        bold: true,
        fg: Color::Ansi(1),
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::styled("err", style)],
    };
    term.apply(&[RowUpdate::Write { row: 0, line }]);
    assert_eq!(term.styled(), "[err]{bold,fg=Ansi(1)}");
}

#[test]
fn styled_snapshot_of_plain_text_has_no_markers() {
    let mut term = TestTerminal::new(80);
    term.apply(&[RowUpdate::Write {
        row: 0,
        line: Line::raw("plain"),
    }]);
    assert_eq!(term.styled(), "plain");
}

mod vt_screen {
    use harness_tui::headless::VtScreen;

    #[test]
    fn line_feed_on_the_last_row_scrolls_into_scrollback() {
        let mut vt = VtScreen::new(10, 2);
        vt.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(vt.scrollback(), &["one".to_string()]);
        assert_eq!(vt.viewport(), vec!["two", "three"]);
    }

    #[test]
    fn cursor_moves_and_erases_follow_the_vt_subset() {
        let mut vt = VtScreen::new(10, 3);
        vt.feed(b"aaaa\r\nbbbb\r\ncccc");
        vt.feed(b"\x1b[2;3H\x1b[0J\x1b[1;1H\x1b[2Kz\x1b[3J");
        assert_eq!(vt.viewport(), vec!["z", "bb", ""]);
        assert!(vt.scrollback().is_empty());
    }

    #[test]
    fn exact_width_rows_wrap_only_on_the_next_printable() {
        let mut vt = VtScreen::new(4, 3);
        // Four cells fill the row; CR LF must not produce an extra blank row.
        vt.feed(b"abcd\r\nef");
        assert_eq!(vt.viewport(), vec!["abcd", "ef", ""]);
        // A fifth printable wraps.
        let mut vt = VtScreen::new(4, 3);
        vt.feed(b"abcde");
        assert_eq!(vt.viewport(), vec!["abcd", "e", ""]);
    }

    #[test]
    fn wide_characters_take_two_cells_and_sgr_is_ignored() {
        let mut vt = VtScreen::new(6, 1);
        vt.feed("\x1b[1;36m好\x1b[0mab".as_bytes());
        assert_eq!(vt.viewport(), vec!["好ab"]);
        assert_eq!(vt.cursor(), (0, 4));
    }
}

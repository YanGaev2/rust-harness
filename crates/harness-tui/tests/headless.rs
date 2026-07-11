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

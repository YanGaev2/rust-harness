use harness_tui::diff::{RowUpdate, diff_frames};
use harness_tui::text::Line;

fn frame(texts: &[&str]) -> Vec<Line> {
    texts.iter().map(|text| Line::raw(*text)).collect()
}

#[test]
fn identical_frames_produce_no_updates() {
    let prev = frame(&["a", "b"]);
    assert_eq!(diff_frames(&prev, &prev.clone()), vec![]);
}

#[test]
fn changed_row_is_rewritten_alone() {
    let prev = frame(&["a", "b", "c"]);
    let next = frame(&["a", "X", "c"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Write {
            row: 1,
            line: Line::raw("X")
        }]
    );
}

#[test]
fn tail_append_touches_only_new_rows() {
    let prev = frame(&["a", "b"]);
    let next = frame(&["a", "b", "c", "d"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![
            RowUpdate::Write {
                row: 2,
                line: Line::raw("c")
            },
            RowUpdate::Write {
                row: 3,
                line: Line::raw("d")
            },
        ]
    );
}

#[test]
fn shrunk_frame_clears_removed_rows() {
    let prev = frame(&["a", "b", "c"]);
    let next = frame(&["a"]);
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Clear { row: 1 }, RowUpdate::Clear { row: 2 }]
    );
}

#[test]
fn empty_prev_is_full_redraw() {
    let next = frame(&["a", "b"]);
    assert_eq!(
        diff_frames(&[], &next),
        vec![
            RowUpdate::Write {
                row: 0,
                line: Line::raw("a")
            },
            RowUpdate::Write {
                row: 1,
                line: Line::raw("b")
            },
        ]
    );
}

#[test]
fn style_only_change_is_detected() {
    use harness_tui::text::{Span, Style};
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let prev = vec![Line::raw("a")];
    let next = vec![Line {
        spans: vec![Span::styled("a", bold)],
    }];
    assert_eq!(
        diff_frames(&prev, &next),
        vec![RowUpdate::Write {
            row: 0,
            line: next[0].clone()
        }]
    );
}

use harness_tui::text::{Line, Span, Style, visible_width};

#[test]
fn width_of_ascii() {
    assert_eq!(visible_width("hello"), 5);
}

#[test]
fn width_of_empty_string_is_zero() {
    assert_eq!(visible_width(""), 0);
}

#[test]
fn width_of_cjk_is_two_columns_per_char() {
    assert_eq!(visible_width("\u{4f60}\u{597d}"), 4); // 你好
}

#[test]
fn width_of_emoji_is_two_columns() {
    assert_eq!(visible_width("\u{1f44d}"), 2); // 👍
}

#[test]
fn width_of_combining_mark_is_zero() {
    assert_eq!(visible_width("e\u{0301}"), 1); // e + combining acute
}

#[test]
fn line_width_sums_span_widths() {
    let line = Line {
        spans: vec![Span::raw("ab"), Span::raw("\u{4f60}")],
    };
    assert_eq!(line.width(), 4);
}

#[test]
fn line_text_concatenates_spans() {
    let line = Line {
        spans: vec![Span::raw("foo"), Span::raw("bar")],
    };
    assert_eq!(line.text(), "foobar");
}

#[test]
fn line_raw_builds_single_plain_span() {
    assert_eq!(
        Line::raw("hi"),
        Line {
            spans: vec![Span::raw("hi")]
        }
    );
}

#[test]
fn span_styled_keeps_style() {
    let style = Style {
        bold: true,
        ..Style::default()
    };
    let span = Span::styled("x", style);
    assert_eq!(span.style, style);
    assert!(!span.style.is_plain());
    assert!(Style::default().is_plain());
}

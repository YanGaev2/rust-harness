use harness_tui::text::{Color, Line, Span, Style, render_ansi, visible_width, wrap};

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

#[test]
fn wrap_short_line_unchanged() {
    assert_eq!(wrap(&Line::raw("hi"), 10), vec![Line::raw("hi")]);
}

#[test]
fn wrap_empty_line_stays_one_empty_line() {
    assert_eq!(wrap(&Line::default(), 10), vec![Line::default()]);
}

#[test]
fn wrap_breaks_at_space_and_consumes_it() {
    assert_eq!(
        wrap(&Line::raw("hello world"), 5),
        vec![Line::raw("hello"), Line::raw("world")]
    );
}

#[test]
fn wrap_keeps_words_that_fit_together() {
    assert_eq!(
        wrap(&Line::raw("hello world foo"), 11),
        vec![Line::raw("hello world"), Line::raw("foo")]
    );
}

#[test]
fn wrap_moves_whole_word_to_next_line() {
    // Break happens mid-word candidate: "aa bbbb" at width 5 must not
    // emit "aa bb" — the whole word moves down.
    assert_eq!(
        wrap(&Line::raw("aa bbbb"), 5),
        vec![Line::raw("aa"), Line::raw("bbbb")]
    );
}

#[test]
fn wrap_hard_breaks_long_word() {
    assert_eq!(
        wrap(&Line::raw("abcdefghij"), 4),
        vec![Line::raw("abcd"), Line::raw("efgh"), Line::raw("ij")]
    );
}

#[test]
fn wrap_never_splits_wide_cjk_char() {
    assert_eq!(
        wrap(&Line::raw("\u{4f60}\u{597d}\u{4e16}\u{754c}"), 5), // 你好世界
        vec![Line::raw("\u{4f60}\u{597d}"), Line::raw("\u{4e16}\u{754c}")]
    );
}

#[test]
fn wrap_never_splits_emoji() {
    assert_eq!(
        wrap(&Line::raw("\u{1f44d}\u{1f44d}\u{1f44d}"), 5), // 👍👍👍
        vec![Line::raw("\u{1f44d}\u{1f44d}"), Line::raw("\u{1f44d}")]
    );
}

#[test]
fn wrap_drops_trailing_space_line() {
    assert_eq!(wrap(&Line::raw("hello "), 5), vec![Line::raw("hello")]);
}

#[test]
fn wrap_preserves_styles_across_break() {
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("hello "), Span::styled("world", bold)],
    };
    assert_eq!(
        wrap(&line, 5),
        vec![
            Line::raw("hello"),
            Line {
                spans: vec![Span::styled("world", bold)]
            }
        ]
    );
}

#[test]
fn wrap_zero_width_returns_line_unchanged() {
    assert_eq!(wrap(&Line::raw("abc"), 0), vec![Line::raw("abc")]);
}

#[test]
fn sgr_of_plain_style_is_empty() {
    assert_eq!(Style::default().sgr(), "");
}

#[test]
fn sgr_bold() {
    let style = Style {
        bold: true,
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[1m");
}

#[test]
fn sgr_reverse() {
    let style = Style {
        reverse: true,
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[7m");
}

#[test]
fn sgr_ansi_foreground_normal_and_bright() {
    let normal = Style {
        fg: Color::Ansi(1),
        ..Style::default()
    };
    assert_eq!(normal.sgr(), "\x1b[31m");
    let bright = Style {
        fg: Color::Ansi(9),
        ..Style::default()
    };
    assert_eq!(bright.sgr(), "\x1b[91m");
}

#[test]
fn sgr_indexed_foreground_and_rgb_background() {
    let indexed = Style {
        fg: Color::Indexed(196),
        ..Style::default()
    };
    assert_eq!(indexed.sgr(), "\x1b[38;5;196m");
    let rgb_bg = Style {
        bg: Color::Rgb(1, 2, 3),
        ..Style::default()
    };
    assert_eq!(rgb_bg.sgr(), "\x1b[48;2;1;2;3m");
}

#[test]
fn sgr_combines_codes_in_fixed_order() {
    let style = Style {
        bold: true,
        underline: true,
        fg: Color::Ansi(2),
        ..Style::default()
    };
    assert_eq!(style.sgr(), "\x1b[1;4;32m");
}

#[test]
fn render_ansi_plain_line_is_passthrough() {
    assert_eq!(render_ansi(&Line::raw("hi")), "hi");
}

#[test]
fn render_ansi_wraps_styled_span_with_reset() {
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let line = Line {
        spans: vec![Span::raw("a"), Span::styled("b", bold), Span::raw("c")],
    };
    assert_eq!(render_ansi(&line), "a\x1b[1mb\x1b[0mc");
}

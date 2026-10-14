use harness_tui::components::editor::Editor;
use harness_tui::components::menu::{MenuItem, menu_lines};
use harness_tui::components::reveal::{FADE_CHARS, FADE_OUT_SECS, LAG_SECS, Reveal, fade_in};
use harness_tui::components::select::select_lines;
use harness_tui::components::spinner::{format_elapsed, shimmer, working_line};
use harness_tui::components::status::status_line;
use harness_tui::text::{Color, Line, Span, Style};
use harness_tui::theme::Theme;
use std::time::{Duration, Instant};

fn plain(lines: &[Line]) -> Vec<String> {
    lines.iter().map(Line::text).collect()
}

fn styled(line: &Line) -> String {
    line.spans
        .iter()
        .map(|span: &Span| {
            if span.style.is_plain() {
                span.text.clone()
            } else if span.style.reverse {
                format!("[{}]", span.text)
            } else {
                span.text.clone()
            }
        })
        .collect()
}

fn editor() -> Editor {
    Editor::new("> ", "Type a message")
}

// --- Editor ---

#[test]
fn editor_inserts_and_moves() {
    let mut ed = editor();
    ed.insert_str("hello");
    assert_eq!(ed.text(), "hello");
    assert_eq!(ed.cursor(), 5);
    ed.move_left();
    ed.insert_char('X');
    assert_eq!(ed.text(), "hellXo");
    ed.move_home();
    assert_eq!(ed.cursor(), 0);
    ed.move_end();
    assert_eq!(ed.cursor(), 6);
}

#[test]
fn editor_backspace_removes_grapheme_before_cursor() {
    let mut ed = editor();
    ed.insert_str("ab");
    ed.backspace();
    assert_eq!(ed.text(), "a");
    ed.insert_str("\u{1f44d}");
    ed.backspace();
    assert_eq!(ed.text(), "a");
}

#[test]
fn editor_take_clears_and_returns() {
    let mut ed = editor();
    ed.insert_str("msg");
    assert_eq!(ed.take(), "msg");
    assert!(ed.is_empty());
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn editor_renders_prompt_text_and_caret() {
    let mut ed = editor();
    ed.insert_str("hi");
    let lines = ed.render(20, 6);
    assert_eq!(lines.len(), 1);
    assert_eq!(styled(&lines[0]), " > hi[ ]");
}

#[test]
fn editor_caret_sits_on_the_char_under_cursor() {
    let mut ed = editor();
    ed.insert_str("abc");
    ed.move_left(); // caret on 'c'
    let lines = ed.render(20, 6);
    assert_eq!(styled(&lines[0]), " > ab[c]");
}

#[test]
fn editor_renders_placeholder_when_empty() {
    let ed = editor();
    let lines = ed.render(40, 6);
    assert_eq!(lines.len(), 1);
    let text = lines[0].text();
    assert!(text.starts_with(" > "));
    assert!(text.contains("Type a message"));
}

#[test]
fn editor_wraps_to_content_width() {
    let mut ed = editor();
    ed.insert_str("abcdef");
    // width 7 - prefix 3 - right pad 1 = 3 content columns.
    let lines = ed.render(7, 6);
    assert_eq!(plain(&lines), vec![" > abc", "   def "]);
}

#[test]
fn editor_newline_starts_a_new_row() {
    let mut ed = editor();
    ed.insert_str("ab\ncd");
    let lines = ed.render(20, 6);
    assert_eq!(plain(&lines), vec![" > ab", "   cd "]);
}

#[test]
fn editor_window_follows_the_caret() {
    let mut ed = editor();
    ed.insert_str("a\nb\nc\nd");
    let lines = ed.render(20, 2);
    // Four logical rows, window of 2 ending at the caret row.
    assert_eq!(plain(&lines), vec!["   c", "   d "]);
}

#[test]
fn editor_rows_reports_full_height() {
    let mut ed = editor();
    ed.insert_str("a\nb\nc");
    assert_eq!(ed.rows(20), 3);
    assert_eq!(editor().rows(20), 1);
}

// --- Spinner ---

#[test]
fn working_line_shows_glyph_label_and_detail() {
    let line = working_line(&Theme::dark(), 0, "5s · esc to interrupt", 0.5);
    assert_eq!(line.text(), " ⠋ Working… (5s · esc to interrupt)");
}

#[test]
fn working_line_without_detail_drops_the_parentheses() {
    let line = working_line(&Theme::dark(), 3, "", 0.0);
    assert!(line.text().ends_with("Working…"), "{:?}", line.text());
}

#[test]
fn shimmer_brightens_a_band_that_moves_with_the_phase() {
    let base = Color::Rgb(0x80, 0x80, 0x80);
    let bright = Color::Rgb(0xff, 0xff, 0xff);
    let brightest = |phase: f32| {
        let spans = shimmer("Working…", base, bright, phase);
        assert_eq!(
            spans.iter().map(|s| s.text.as_str()).collect::<String>(),
            "Working…"
        );
        (0..spans.len())
            .max_by_key(|&i| match spans[i].style.fg {
                Color::Rgb(r, _, _) => r,
                _ => 0,
            })
            .unwrap()
    };
    assert!(
        brightest(0.3) < brightest(0.7),
        "the band sweeps left to right"
    );
    // Outside the band every character keeps the base color.
    let at_start = shimmer("Working…", base, bright, 0.0);
    assert!(
        at_start.iter().all(|span| span.style.fg == base),
        "{at_start:?}"
    );
    // Palette colors cannot blend: one plain span.
    assert_eq!(shimmer("Working…", Color::Ansi(8), bright, 0.5).len(), 1);
}

// --- Reveal ---

#[test]
fn reveal_paces_a_burst_instead_of_printing_it_at_once() {
    let text = "x".repeat(600);
    let t0 = Instant::now();
    let mut reveal = Reveal::new();
    assert_eq!(
        reveal.advance(&text, t0),
        "",
        "the first call starts the clock"
    );
    let frame = Duration::from_millis(33);
    let first = reveal.advance(&text, t0 + frame).len();
    assert!(first > 0 && first < 120, "one frame shows a slice: {first}");
    // The pace follows the backlog: the whole burst shows within a few
    // multiples of the lag, never instantly and never stuck.
    let mut now = t0 + frame;
    let mut frames = 1;
    while !reveal.is_done(&text) {
        now += frame;
        reveal.advance(&text, now);
        frames += 1;
        assert!(frames < 200, "the reveal must finish");
    }
    let secs = frames as f64 * 0.033;
    assert!(secs > LAG_SECS && secs < LAG_SECS * 12.0, "{secs}s");
}

#[test]
fn reveal_keeps_up_with_text_that_keeps_arriving_and_respects_char_boundaries() {
    let mut text = String::new();
    let t0 = Instant::now();
    let mut reveal = Reveal::new();
    reveal.advance(&text, t0);
    for frame in 1..=60u32 {
        text.push_str("привет мир ");
        let shown = reveal.advance(&text, t0 + Duration::from_millis(33) * frame);
        assert!(text.starts_with(shown));
    }
    let backlog = text[reveal.shown()..].chars().count();
    assert!(
        backlog < 200,
        "the display trails the stream, not more: {backlog}"
    );
}

#[test]
fn finished_reveal_fades_its_tail_out_then_stops_animating() {
    let text = "done";
    let t0 = Instant::now();
    let mut reveal = Reveal::new();
    reveal.advance(text, t0);
    reveal.advance(text, t0 + Duration::from_secs(1));
    assert!(reveal.is_done(text));
    let done = t0 + Duration::from_secs(1);
    assert_eq!(reveal.fade_len(text, done), FADE_CHARS);
    let half = done + Duration::from_secs_f64(FADE_OUT_SECS / 2.0);
    assert!(reveal.fade_len(text, half) < FADE_CHARS);
    let after = done + Duration::from_secs_f64(FADE_OUT_SECS * 2.0);
    assert_eq!(reveal.fade_len(text, after), 0);
    assert!(!reveal.is_animating(text, after));
}

#[test]
fn fade_in_steps_the_newest_characters_from_the_background() {
    let background = (0, 0, 0);
    let foreground = (200, 200, 200);
    let mut lines = vec![
        Line::raw("old row"),
        Line {
            spans: vec![
                Span::raw("ab "),
                Span::styled("cd", Style::fg(Color::Rgb(100, 0, 0))),
            ],
        },
    ];
    fade_in(&mut lines, 3, background, foreground);
    assert_eq!(lines[0], Line::raw("old row"), "older rows are untouched");
    let colors: Vec<Color> = lines[1]
        .spans
        .iter()
        .flat_map(|span| span.text.chars().map(move |_| span.style.fg))
        .collect();
    // "a" is outside the faded tail, the space is skipped, then b, c, d
    // step toward the background (d, the newest, is the darkest).
    assert_eq!(colors[0], Color::Default);
    assert_eq!(colors[2], Color::Default, "spaces are not faded");
    assert_eq!(colors[1], Color::Rgb(150, 150, 150), "b: 3/4 of the way");
    assert_eq!(colors[3], Color::Rgb(50, 0, 0), "c: 2/4 toward its own red");
    assert_eq!(colors[4], Color::Rgb(25, 0, 0), "d: 1/4");
    assert_eq!(lines[1].text(), "ab cd");
}

#[test]
fn format_elapsed_renders_seconds_and_minutes() {
    assert_eq!(format_elapsed(45), "45s");
    assert_eq!(format_elapsed(125), "2m 5s");
}

// --- Status line ---

#[test]
fn status_joins_left_and_right_when_they_fit() {
    let line = status_line(40, Line::raw(" provider/model"), Line::raw("Enter send "));
    let text = line.text();
    assert!(text.starts_with(" provider/model"));
    assert!(text.ends_with("Enter send "));
    assert_eq!(harness_tui::text::visible_width(&text), 40);
}

#[test]
fn status_drops_right_hint_when_too_narrow() {
    let line = status_line(10, Line::raw(" provider"), Line::raw("hints "));
    assert_eq!(line.text(), " provider");
}

#[test]
fn status_clips_an_overlong_left_side_with_an_ellipsis() {
    let line = status_line(12, Line::raw(" F:/very/long/path"), Line::raw("model "));
    assert_eq!(line.text(), " F:/very/lo…");
    assert_eq!(harness_tui::text::visible_width(&line.text()), 12);
}

// --- Completion menu ---

fn items() -> Vec<MenuItem> {
    vec![
        MenuItem {
            name: "/model".into(),
            usage: "/model PROVIDER MODEL".into(),
            description: "switch the active provider/model".into(),
        },
        MenuItem {
            name: "/new".into(),
            usage: "/new".into(),
            description: "start a fresh chat session".into(),
        },
    ]
}

#[test]
fn menu_marks_the_selected_row() {
    let lines = menu_lines(&Theme::dark(), &items(), "/", 1, 6);
    let texts = plain(&lines);
    assert_eq!(texts.len(), 2);
    assert!(texts[0].starts_with("   /model"));
    assert!(texts[1].starts_with(" \u{203a} /new"));
    assert!(texts[0].contains("switch the active provider/model"));
}

#[test]
fn menu_caps_visible_rows() {
    let many: Vec<MenuItem> = (0..10)
        .map(|i| MenuItem {
            name: format!("/cmd{i}"),
            usage: format!("/cmd{i}"),
            description: "desc".into(),
        })
        .collect();
    assert_eq!(menu_lines(&Theme::dark(), &many, "/", 0, 6).len(), 6);
}

// --- Select list ---

#[test]
fn select_marks_the_chosen_item() {
    let lines = select_lines(&["codex", "deepseek"], 1);
    assert_eq!(plain(&lines), vec!["  codex", "> deepseek"]);
}

// --- Editor motions and kill commands ---

#[test]
fn editor_deletes_forward_and_by_word() {
    let mut ed = editor();
    ed.insert_str("hello big world");
    ed.delete_word_back();
    assert_eq!(ed.text(), "hello big ");
    ed.delete_word_back();
    assert_eq!(ed.text(), "hello ");
    ed.move_home();
    ed.delete_forward();
    assert_eq!(ed.text(), "ello ");
    assert_eq!(ed.cursor(), 0);
}

#[test]
fn editor_word_motions_skip_spaces_then_words() {
    let mut ed = editor();
    ed.insert_str("one two  three");
    ed.move_word_left();
    assert_eq!(ed.cursor(), 9);
    ed.move_word_left();
    assert_eq!(ed.cursor(), 4);
    ed.move_word_right();
    assert_eq!(ed.cursor(), 7);
}

#[test]
fn editor_line_commands_stay_on_the_caret_line() {
    let mut ed = editor();
    ed.insert_str("first\nsecond line");
    ed.move_home();
    assert_eq!(ed.cursor(), 6, "Home goes to the start of the current line");
    ed.move_word_right();
    ed.delete_to_line_end();
    assert_eq!(ed.text(), "first\nsecond");
    ed.delete_to_line_start();
    assert_eq!(ed.text(), "first\n");
}

#[test]
fn editor_up_down_keep_the_column_and_report_the_edges() {
    let mut ed = editor();
    ed.insert_str("abcdef\nxy\n12345");
    assert!(ed.move_up());
    assert_eq!(ed.cursor(), 9, "column clamps to the shorter line");
    assert!(ed.move_up());
    assert_eq!(ed.cursor(), 2);
    assert!(!ed.move_up(), "first line reports the edge");
    assert!(ed.move_down());
    assert!(ed.move_down());
    assert!(!ed.move_down(), "last line reports the edge");
}

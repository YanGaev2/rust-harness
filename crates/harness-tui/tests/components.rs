use harness_tui::components::editor::Editor;
use harness_tui::components::menu::{MenuItem, menu_lines};
use harness_tui::components::select::select_lines;
use harness_tui::components::spinner::{format_elapsed, spinner_lines};
use harness_tui::components::status::status_line;
use harness_tui::text::{Line, Span};

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
fn spinner_shows_elapsed_with_spacer() {
    let lines = spinner_lines(0, 5);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text(), "");
    assert!(lines[1].text().contains("Working… (5s)"));
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
    let lines = menu_lines(&items(), "/", 1, 6);
    let texts = plain(&lines);
    assert_eq!(texts.len(), 2);
    assert!(texts[0].starts_with("   /model"));
    assert!(texts[1].starts_with(" \u{25b8} /new"));
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
    assert_eq!(menu_lines(&many, "/", 0, 6).len(), 6);
}

// --- Select list ---

#[test]
fn select_marks_the_chosen_item() {
    let lines = select_lines(&["codex", "deepseek"], 1);
    assert_eq!(plain(&lines), vec!["  codex", "> deepseek"]);
}

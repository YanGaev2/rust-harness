use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use harness_tui::core::Screen;
use harness_tui::terminal::{Terminal, esc};
use harness_tui::text::Line;

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

fn screen(width: u16, height: u16, start_row: u16) -> (Screen, SharedBuf) {
    let buf = SharedBuf::default();
    let terminal = Terminal::with_backend(Box::new(buf.clone()));
    (Screen::new(terminal, width, height, start_row), buf)
}

fn lines(texts: &[&str]) -> Vec<Line> {
    texts.iter().map(|t| Line::raw(*t)).collect()
}

#[test]
fn panel_draws_at_start_row_when_screen_is_empty() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let out = buf.contents();
    assert!(out.contains("\x1b[1;1H")); // row 0 → escape row 1
    assert!(out.contains("input"));
    assert!(out.contains("\x1b[2;1H"));
    assert!(out.contains("status"));
}

#[test]
fn emit_prints_content_and_repaints_panel_below() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["panel"])).unwrap();
    let before = buf.contents().len();
    screen.emit(&lines(&["one", "two", "three"])).unwrap();
    let out = buf.contents()[before..].to_string();
    // Content starts where the panel was, panel is cleared first.
    assert!(out.contains("\x1b[1;1H"));
    assert!(out.contains(esc::CLEAR_DOWN));
    assert!(out.contains("one\r\ntwo\r\nthree\r\n"));
    // Panel repainted right below the content (row 3 → escape row 4).
    assert!(out.contains("\x1b[4;1H"));
    assert!(out.contains("panel"));
}

#[test]
fn emit_scrolls_when_content_reaches_the_panel_reserve() {
    let (mut screen, buf) = screen(40, 6, 0);
    screen.render_panel(lines(&["p1", "p2"])).unwrap();
    let texts: Vec<String> = (0..8).map(|i| format!("line{i}")).collect();
    let refs: Vec<Line> = texts.iter().map(Line::raw).collect();
    let before = buf.contents().len();
    screen.emit(&refs).unwrap();
    let out = buf.contents()[before..].to_string();
    // Panel is pinned at the bottom reserve: height 6 - panel 2 = row 4.
    assert!(out.contains("\x1b[5;1H"));
    assert!(out.contains("p1"));
    // All content lines were printed (they live in scrollback now).
    for text in &texts {
        assert!(out.contains(text.as_str()), "missing {text}");
    }
}

#[test]
fn emitting_again_appends_below_previous_content() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["panel"])).unwrap();
    screen.emit(&lines(&["a"])).unwrap();
    let before = buf.contents().len();
    screen.emit(&lines(&["b"])).unwrap();
    let out = buf.contents()[before..].to_string();
    // Second emission starts at row 1 (below "a"), panel lands at row 2.
    assert!(out.contains("\x1b[2;1H"));
    assert!(out.contains("b\r\n"));
    assert!(out.contains("\x1b[3;1H"));
}

#[test]
fn same_height_panel_update_diffs_rows() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["same", "old status"])).unwrap();
    let before = buf.contents().len();
    screen.render_panel(lines(&["same", "new status"])).unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(out.contains("new status"));
    // The unchanged first row is not rewritten.
    assert!(!out.contains("same"));
}

#[test]
fn growing_panel_is_fully_redrawn() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["one"])).unwrap();
    let before = buf.contents().len();
    screen
        .render_panel(lines(&["one", "two", "three"]))
        .unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(out.contains("one"));
    assert!(out.contains("two"));
    assert!(out.contains("three"));
}

#[test]
fn resize_pins_panel_to_bottom_and_redraws() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let before = buf.contents().len();
    screen.resize(40, 5).unwrap();
    let out = buf.contents()[before..].to_string();
    // Height 5, panel 2 → origin row 3 (escape row 4).
    assert!(out.contains("\x1b[4;1H"));
    assert!(out.contains("input"));
    assert!(out.contains("status"));
    assert_eq!(screen.width(), 40);
}

#[test]
fn width_is_exposed_for_layout() {
    let (screen, _) = screen(72, 20, 0);
    assert_eq!(screen.width(), 72);
    assert_eq!(screen.height(), 20);
}

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
fn panel_pins_to_the_bottom_even_on_an_empty_screen() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let out = buf.contents();
    // Height 10, panel 2 → rows 8 and 9 (escape rows 9 and 10).
    assert!(out.contains("\x1b[9;1H"));
    assert!(out.contains("input"));
    assert!(out.contains("\x1b[10;1H"));
    assert!(out.contains("status"));
}

#[test]
fn emit_prints_content_and_repaints_panel_below() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["panel"])).unwrap();
    let before = buf.contents().len();
    screen.emit(&lines(&["one", "two", "three"])).unwrap();
    let out = buf.contents()[before..].to_string();
    // Content starts at the top; the old panel area is cleared first.
    assert!(out.contains("\x1b[1;1H"));
    assert!(out.contains(esc::CLEAR_DOWN));
    assert!(out.contains("one\r\ntwo\r\nthree\r\n"));
    // Panel stays pinned to the bottom row (height 10 → escape row 10).
    assert!(out.contains("\x1b[10;1H"));
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
    // Second emission starts at row 1 (below "a"); the panel stays at
    // the bottom (height 10 → escape row 10).
    assert!(out.contains("\x1b[2;1H"));
    assert!(out.contains("b\r\n"));
    assert!(out.contains("\x1b[10;1H"));
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

#[test]
fn emit_with_no_panel_keeps_origin_on_screen() {
    let (mut screen, buf) = screen(40, 5, 4);
    // No panel painted yet; emit two lines starting at the bottom row.
    screen.emit(&lines(&["a", "b"])).unwrap();
    screen.render_panel(lines(&["p"])).unwrap();
    let out = buf.contents();
    // The panel lands on the last row (4 → escape 5), never outside.
    assert!(out.contains("\x1b[5;1H"));
    assert!(!out.contains("\x1b[6;1H"));
}

#[test]
fn takeover_scrolls_shell_content_away_and_starts_at_the_top() {
    // The shell left its banner on screen; the cursor sits on row 3.
    let (mut screen, buf) = screen(40, 6, 3);
    screen.takeover().unwrap();
    let out = buf.contents();
    // Newlines from the bottom row push the shell text into native
    // scrollback (still reachable by scrolling up, unlike CLEAR_ALL).
    assert!(out.contains("\x1b[6;1H"), "must scroll from the bottom row");
    assert!(out.contains(&"\r\n".repeat(6)), "one newline per row");
    // The app now owns the window: content starts at the very top.
    let before = buf.contents().len();
    screen.emit(&lines(&["hello"])).unwrap();
    let emitted = buf.contents()[before..].to_string();
    assert!(
        emitted.contains("\x1b[1;1H"),
        "content not at top: {emitted:?}"
    );
}

#[test]
fn clear_wipes_screen_and_scrollback_and_resets_origin() {
    let (mut screen, buf) = screen(40, 10, 5);
    screen.render_panel(lines(&["input", "status"])).unwrap();
    screen.emit(&lines(&["old chat"])).unwrap();
    let before = buf.contents().len();
    screen.clear().unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(out.contains("\x1b[2J"), "missing screen wipe: {out:?}");
    assert!(out.contains("\x1b[3J"), "missing scrollback wipe: {out:?}");
    // The panel is forgotten: the next paint is a full redraw pinned to
    // the bottom (height 10, panel 2 → escape row 9).
    let after_clear = buf.contents().len();
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let repaint = buf.contents()[after_clear..].to_string();
    assert!(
        repaint.contains("\x1b[9;1H"),
        "panel not pinned to bottom: {repaint:?}"
    );
    assert!(repaint.contains("input"));
    assert!(repaint.contains("status"));
}

#[test]
fn oversized_panel_is_tail_clipped_without_panicking() {
    let (mut screen, buf) = screen(40, 3, 0);
    let texts: Vec<String> = (0..6).map(|i| format!("row{i}")).collect();
    let panel: Vec<Line> = texts.iter().map(Line::raw).collect();
    screen.render_panel(panel).unwrap();
    let out = buf.contents();
    // Only the tail rows are painted; the head is dropped.
    assert!(out.contains("row5"));
    assert!(!out.contains("row0"));
    // Shrinking the screen further must not panic either.
    screen.resize(40, 2).unwrap();
}

use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use harness_tui::fullscreen::FullScreen;
use harness_tui::headless::VtScreen;
use harness_tui::terminal::{Terminal, esc};
use harness_tui::text::Line;

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap())
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

/// A full screen whose bytes feed a VT screen that already shows a shell
/// prompt on its first row.
fn screen(width: u16, height: u16) -> (FullScreen, SharedBuf, VtScreen) {
    let buf = SharedBuf::default();
    let mut vt = VtScreen::new(width, height);
    vt.feed(b"$ harness\r\n");
    let terminal = Terminal::with_backend(Box::new(buf.clone()));
    let screen = FullScreen::new(terminal, width, height).unwrap();
    vt.feed(&buf.take());
    (screen, buf, vt)
}

fn lines(texts: &[&str]) -> Vec<Line> {
    texts.iter().map(|text| Line::raw(*text)).collect()
}

#[test]
fn entering_switches_to_the_alternate_screen_and_captures_the_mouse() {
    let buf = SharedBuf::default();
    let terminal = Terminal::with_backend(Box::new(buf.clone()));
    let _screen = FullScreen::new(terminal, 20, 5).unwrap();
    let out = String::from_utf8(buf.take()).unwrap();
    assert!(out.contains(esc::ALT_SCREEN_ON), "{out:?}");
    assert!(out.contains(esc::MOUSE_ON), "{out:?}");

    let (_screen, _buf, vt) = screen(20, 5);
    assert!(vt.in_alt_screen());
    assert!(vt.viewport().iter().all(String::is_empty), "a blank page");
}

#[test]
fn draw_pins_the_frame_and_rewrites_only_changed_rows() {
    let (mut screen, buf, mut vt) = screen(20, 5);
    screen
        .draw(lines(&["history", "", "› prompt", "status"]))
        .unwrap();
    vt.feed(&buf.take());
    assert_eq!(vt.viewport(), vec!["history", "", "› prompt", "status", ""]);

    screen
        .draw(lines(&["history", "", "› prompt", "status"]))
        .unwrap();
    assert!(buf.take().is_empty(), "an unchanged frame writes nothing");

    screen
        .draw(lines(&["history", "", "› prompt!", "status"]))
        .unwrap();
    let out = String::from_utf8(buf.take()).unwrap();
    assert!(out.contains("prompt!"));
    assert!(!out.contains("history"), "unchanged rows stay: {out:?}");
}

#[test]
fn frames_taller_than_the_screen_are_cut_and_nothing_scrolls() {
    let (mut screen, buf, mut vt) = screen(20, 3);
    screen.draw(lines(&["a", "b", "c", "d", "e"])).unwrap();
    vt.feed(&buf.take());
    assert_eq!(vt.viewport(), vec!["a", "b", "c"]);
    assert!(vt.scrollback().is_empty());
}

#[test]
fn resize_repaints_every_row_at_the_new_size() {
    let (mut screen, buf, _vt) = screen(20, 4);
    screen.draw(lines(&["one", "two"])).unwrap();
    buf.take();
    screen.resize(30, 6).unwrap();
    screen.draw(lines(&["one", "two"])).unwrap();
    let mut vt = VtScreen::new(30, 6);
    vt.feed(&buf.take());
    assert_eq!(vt.viewport()[..2], ["one", "two"]);
    assert_eq!(screen.width(), 30);
    assert_eq!(screen.height(), 6);
}

#[test]
fn leave_restores_the_shell_screen_and_print_adds_native_scrollback() {
    let (mut screen, buf, mut vt) = screen(20, 4);
    screen.draw(lines(&["chat frame"])).unwrap();
    screen.leave().unwrap();
    vt.feed(&buf.take());
    assert!(!vt.in_alt_screen());
    assert_eq!(vt.viewport()[0], "$ harness", "the shell screen is back");
    assert!(!vt.history().iter().any(|row| row == "chat frame"));

    screen
        .print(&lines(&["› question", "answer", "", "end"]))
        .unwrap();
    vt.feed(&buf.take());
    assert_eq!(
        vt.history(),
        vec!["$ harness", "› question", "answer", "", "end", ""],
        "printed below the prompt, older rows in scrollback"
    );

    // Leaving twice is harmless.
    screen.leave().unwrap();
    assert!(buf.take().is_empty());
}

#[test]
fn drop_leaves_the_alternate_screen() {
    let (screen, buf, mut vt) = screen(20, 4);
    drop(screen);
    let out = buf.take();
    vt.feed(&out);
    assert!(!vt.in_alt_screen());
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains(esc::MOUSE_OFF), "{out:?}");
}

#[test]
fn copy_sends_the_text_to_the_clipboard_with_osc_52() {
    let (mut screen, buf, _vt) = screen(20, 4);
    screen.copy("hi\nthere").unwrap();
    let out = String::from_utf8(buf.take()).unwrap();
    assert_eq!(out, "\x1b]52;c;aGkKdGhlcmU=\x07");
}

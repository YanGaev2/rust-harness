use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use harness_tui::core::Screen;
use harness_tui::headless::VtScreen;
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
fn resize_keeps_the_flow_origin_instead_of_bottom_pinning() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.render_panel(lines(&["input", "status"])).unwrap();
    screen.emit(&lines(&["one", "two"])).unwrap();
    // Panel now follows content at row 2. Growing the window must NOT
    // teleport it to the bottom (codex finding: resize reinstated the
    // bottom-pinned layout).
    let before = buf.contents().len();
    screen.resize(40, 20).unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(
        out.contains("\x1b[3;1H"),
        "panel must stay at row 2: {out:?}"
    );
    assert!(
        !out.contains("\x1b[19;1H"),
        "panel must not be bottom-pinned: {out:?}"
    );
    assert_eq!(screen.width(), 40);

    // Shrinking below the flow clamps the origin so the panel still fits.
    let before = buf.contents().len();
    screen.resize(40, 3).unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(out.contains("\x1b[2;1H"), "clamped to fit: {out:?}");
}

#[test]
fn present_commits_and_paints_live_in_one_synchronized_frame() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen
        .render_panel(lines(&["old spinner", "old editor", "old status"]))
        .unwrap();
    let before = buf.contents().len();
    screen
        .present(
            &lines(&["> user message"]),
            lines(&["spinner", "editor", "status"]),
            0,
        )
        .unwrap();
    let out = buf.contents()[before..].to_string();
    // One synchronized frame — commit and repaint are atomic.
    assert_eq!(out.matches("\x1b[?2026h").count(), 1, "{out:?}");
    assert_eq!(out.matches("\x1b[?2026l").count(), 1);
    // The committed row lands at the old flow origin (row 0)…
    assert!(out.contains("> user message"));
    assert!(out.contains("\x1b[1;1H"));
    // …the NEW live frame starts directly below it (row 1)…
    assert!(out.contains("\x1b[2;1H"));
    assert!(out.contains("spinner"));
    // …and the stale panel snapshot is never repainted.
    assert!(
        !out.contains("old spinner"),
        "stale frame repainted: {out:?}"
    );
}

#[test]
fn present_reserves_rows_for_the_new_live_frame_not_the_old() {
    // Height 6; flow at row 4 after some content.
    let (mut screen, buf) = screen(40, 6, 0);
    screen.emit(&lines(&["a", "b", "c", "d"])).unwrap();
    // Old live frame is tall (4 rows), the next one is short (1 row).
    screen
        .render_panel(lines(&["s1", "s2", "s3", "s4"]))
        .unwrap();
    let before = buf.contents().len();
    // Committing one row with a 1-row next frame must scroll by the NEW
    // frame's need only (codex finding: reserve came from the old panel).
    screen
        .present(&lines(&["done"]), lines(&["editor"]), 0)
        .unwrap();
    let out = buf.contents()[before..].to_string();
    // Flow sat at row 2; the commit ends at row 3 and the 1-row live
    // frame fits there — no scroll burst from the bottom row (which
    // would show up as a move to the last row followed by newlines).
    assert!(!out.contains("\x1b[6;1H"), "over-scrolled: {out:?}");
    assert!(out.contains("done"));
    assert!(out.contains("editor"));
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
fn clear_screen_starts_the_tui_at_the_top_without_touching_scrollback() {
    // The shell prompt sits mid-screen at startup.
    let (mut screen, buf) = screen(40, 10, 5);
    screen.clear_screen().unwrap();
    let out = buf.contents();
    // Visible viewport wiped, cursor homed — like pi's clearScreen()
    // and Claude Code at startup.
    assert!(out.contains("\x1b[2J"), "missing viewport wipe: {out:?}");
    assert!(
        !out.contains("\x1b[3J"),
        "startup must not destroy the user's scrollback: {out:?}"
    );
    // The TUI now draws from the very top: content first, panel below.
    let before = buf.contents().len();
    screen.emit(&lines(&["banner"])).unwrap();
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let drawn = buf.contents()[before..].to_string();
    assert!(drawn.contains("\x1b[1;1H"), "content not at top: {drawn:?}");
    // Panel follows the content directly (row 1 → escape row 2).
    assert!(drawn.contains("\x1b[2;1H"));
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
    // The panel is forgotten: the next paint is a full redraw at row 0.
    let after_clear = buf.contents().len();
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let repaint = buf.contents()[after_clear..].to_string();
    assert!(
        repaint.contains("\x1b[1;1H"),
        "panel not at top: {repaint:?}"
    );
    assert!(repaint.contains("input"));
    assert!(repaint.contains("status"));
}

// --- docked layout (chat TUI): content flows top-down, the editor and
// status rows sit on the bottom edge, blank filler between them belongs
// to the panel. Asserted end-to-end through the VT emulator: what the
// user can scroll back to must never contain filler gaps or duplicates.

/// Screen + VT emulator pair of the same geometry.
fn vt_screen(width: u16, height: u16) -> (Screen, SharedBuf, VtScreen) {
    let (screen, buf) = screen(width, height, 0);
    (screen, buf, VtScreen::new(width, height))
}

fn replay(vt: &mut VtScreen, buf: &SharedBuf, cursor: &mut usize) {
    let bytes = buf.0.lock().unwrap().clone();
    vt.feed(&bytes[*cursor..]);
    *cursor = bytes.len();
}

/// History with trailing blank rows dropped.
fn history(vt: &VtScreen) -> Vec<String> {
    let mut rows = vt.history();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows
}

#[test]
fn docked_rows_sit_on_the_bottom_edge_below_top_flowing_content() {
    let (mut screen, buf, mut vt) = vt_screen(40, 10);
    screen.clear_screen().unwrap();
    screen
        .present(&lines(&["banner"]), lines(&["editor", "status"]), 2)
        .unwrap();
    let mut cursor = 0;
    replay(&mut vt, &buf, &mut cursor);
    let rows = vt.viewport();
    assert_eq!(rows[0], "banner");
    assert!(rows[1..8].iter().all(String::is_empty), "filler: {rows:?}");
    assert_eq!(rows[8], "editor");
    assert_eq!(rows[9], "status");
    assert!(
        vt.scrollback().is_empty(),
        "startup scrolled: {:?}",
        vt.scrollback()
    );
}

#[test]
fn closing_a_menu_leaves_no_gap_in_history() {
    // The screenshot bug: an 8-row menu opened and closed under the
    // editor, then the next turn committed — the old bottom-pinned
    // layout left 8 blank rows between the answer and the next message.
    let (mut screen, buf, mut vt) = vt_screen(40, 20);
    screen.clear_screen().unwrap();
    let panel = |extra: usize| {
        let mut rows = lines(&["editor-top", "editor", "editor-bottom", "status"]);
        rows.extend((0..extra).map(|i| Line::raw(format!("menu-{i}"))));
        rows
    };
    screen
        .present(&lines(&["answer-0", "answer-1", "answer-2"]), panel(0), 4)
        .unwrap();
    screen.present(&[], panel(8), 12).unwrap();
    screen.present(&[], panel(0), 4).unwrap();
    screen
        .present(&lines(&["user-0", "user-1"]), panel(0), 4)
        .unwrap();
    let mut cursor = 0;
    replay(&mut vt, &buf, &mut cursor);
    let rows = vt.viewport();
    assert_eq!(
        &rows[..5],
        &["answer-0", "answer-1", "answer-2", "user-0", "user-1"],
        "content must stay contiguous: {rows:?}"
    );
    assert_eq!(rows[19], "status");
    assert!(vt.scrollback().is_empty(), "{:?}", vt.scrollback());
}

#[test]
fn live_body_follows_content_and_commits_in_place() {
    let (mut screen, buf, mut vt) = vt_screen(40, 10);
    screen.clear_screen().unwrap();
    screen
        .present(&lines(&["banner"]), lines(&["editor", "status"]), 2)
        .unwrap();
    // A streaming block lives in the panel body, directly under content.
    screen
        .present(&[], lines(&["streaming", "editor", "status"]), 2)
        .unwrap();
    let mut cursor = 0;
    replay(&mut vt, &buf, &mut cursor);
    assert_eq!(vt.viewport()[1], "streaming");
    // Committing it prints it on the very row it was shown — no jump.
    screen
        .present(&lines(&["streaming"]), lines(&["editor", "status"]), 2)
        .unwrap();
    replay(&mut vt, &buf, &mut cursor);
    let rows = vt.viewport();
    assert_eq!(&rows[..2], &["banner", "streaming"]);
    assert!(rows[2..8].iter().all(String::is_empty), "{rows:?}");
    assert_eq!(&rows[8..], &["editor", "status"]);
}

#[test]
fn growing_panel_eats_filler_before_scrolling_content() {
    let (mut screen, buf, mut vt) = vt_screen(40, 10);
    screen.clear_screen().unwrap();
    screen
        .present(&lines(&["a", "b"]), lines(&["editor", "status"]), 2)
        .unwrap();
    // 6 body rows + 2 docked = 8 rows still fit under 2 content rows.
    let mut live: Vec<Line> = (0..6).map(|i| Line::raw(format!("body-{i}"))).collect();
    live.extend(lines(&["editor", "status"]));
    screen.present(&[], live, 2).unwrap();
    let mut cursor = 0;
    replay(&mut vt, &buf, &mut cursor);
    assert!(vt.scrollback().is_empty(), "{:?}", vt.scrollback());
    let rows = vt.viewport();
    assert_eq!(&rows[..3], &["a", "b", "body-0"]);
    assert_eq!(rows[9], "status");
}

#[test]
fn long_session_history_is_exactly_the_committed_rows_in_order() {
    let (mut screen, buf, mut vt) = vt_screen(30, 8);
    screen.clear_screen().unwrap();
    let mut expected = Vec::new();
    for turn in 0..12 {
        // Alternate panel heights (spinner on/off, menu) between commits.
        let mut live: Vec<Line> = (0..turn % 3)
            .map(|i| Line::raw(format!("live-{i}")))
            .collect();
        live.extend(lines(&["editor", "status"]));
        screen.present(&[], live, 2).unwrap();
        let committed = vec![
            Line::raw(format!("turn-{turn}-a")),
            Line::raw(format!("turn-{turn}-b")),
        ];
        expected.extend(committed.iter().map(Line::text));
        screen
            .present(&committed, lines(&["editor", "status"]), 2)
            .unwrap();
    }
    let mut cursor = 0;
    replay(&mut vt, &buf, &mut cursor);
    let mut rows = history(&vt);
    assert_eq!(rows.pop().as_deref(), Some("status"));
    assert_eq!(rows.pop().as_deref(), Some("editor"));
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    assert_eq!(rows, expected, "gaps or duplicates in history");
}

#[test]
fn resize_extends_the_dock_filler_to_the_new_bottom() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.clear_screen().unwrap();
    screen
        .present(&lines(&["banner"]), lines(&["editor", "status"]), 2)
        .unwrap();
    screen.resize(40, 14).unwrap();
    let mut vt = VtScreen::new(40, 14);
    vt.feed(&buf.0.lock().unwrap());
    let rows = vt.viewport();
    assert_eq!(rows[0], "banner", "content untouched: {rows:?}");
    assert_eq!(&rows[12..], &["editor", "status"]);
}

// --- resize: erase + full repaint from the model ---
// Windows Terminal reflows the buffer on width change (and may scroll it
// on height change), so absolute rows recorded before the resize are
// meaningless afterwards. The chat loop erases the viewport eagerly
// (resize_erase) and repaints everything it owns from the model once the
// size settles (repaint).

#[test]
fn resize_erase_wipes_viewport_adopts_size_and_keeps_scrollback() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.clear_screen().unwrap();
    screen.render_panel(lines(&["input", "status"])).unwrap();
    let before = buf.contents().len();
    screen.resize_erase(60, 12).unwrap();
    let out = buf.contents()[before..].to_string();
    assert!(out.contains("\x1b[2J"), "missing viewport wipe: {out:?}");
    assert!(
        !out.contains("\x1b[3J"),
        "resize must never destroy scrollback: {out:?}"
    );
    assert_eq!(screen.width(), 60);
    assert_eq!(screen.height(), 12);
}

#[test]
fn repaint_flows_content_from_the_top_and_docks_the_panel() {
    let (mut screen, buf) = screen(40, 10, 0);
    screen.clear_screen().unwrap();
    screen
        .render_panel(lines(&["old input", "old status"]))
        .unwrap();
    screen.resize_erase(60, 12).unwrap();
    let before = buf.contents().len();
    screen
        .repaint(&lines(&["one", "two"]), lines(&["input", "status"]), 2)
        .unwrap();
    let out = buf.contents()[before..].to_string();
    // One synchronized frame, absolute writes only — repaint must never
    // scroll content into scrollback (that would duplicate history).
    assert_eq!(out.matches("\x1b[?2026h").count(), 1, "{out:?}");
    assert_eq!(out.matches("\x1b[?2026l").count(), 1);
    assert!(
        !out.contains("\r\n"),
        "repaint must not print flow newlines: {out:?}"
    );
    let mut vt = VtScreen::new(60, 12);
    vt.feed(&buf.0.lock().unwrap());
    let rows = vt.viewport();
    assert_eq!(&rows[..2], &["one", "two"]);
    assert!(rows[2..10].iter().all(String::is_empty), "{rows:?}");
    assert_eq!(&rows[10..], &["input", "status"]);
}

#[test]
fn repaint_clips_oversized_content_and_restores_the_diff_origin() {
    let (mut screen, buf) = screen(40, 6, 0);
    screen.clear_screen().unwrap();
    // 8 content lines into 4 free rows (height 6 - panel 2): tail only.
    let texts: Vec<String> = (0..8).map(|i| format!("line{i}")).collect();
    let content: Vec<Line> = texts.iter().map(Line::raw).collect();
    screen
        .repaint(&content, lines(&["input", "status"]), 2)
        .unwrap();
    let out = buf.contents();
    assert!(out.contains("line7"), "tail kept: {out:?}");
    assert!(!out.contains("line3"), "head clipped: {out:?}");
    // Subsequent same-height panel updates diff against the repainted
    // panel at the restored origin (row 4 → escape 5).
    let before = buf.contents().len();
    screen
        .present(&[], lines(&["input", "new status"]), 2)
        .unwrap();
    let update = buf.contents()[before..].to_string();
    assert!(update.contains("\x1b[6;1H"), "diff at origin: {update:?}");
    assert!(update.contains("new status"));
    assert!(!update.contains("line7"), "content repainted: {update:?}");
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

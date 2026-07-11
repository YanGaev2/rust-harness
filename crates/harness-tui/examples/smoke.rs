//! Manual smoke test: `cargo run -p harness-tui --example smoke`.
//! Draws a two-row pinned panel with a bold title, a frame counter,
//! and a drawn caret for ~3 seconds, then restores the terminal.

use std::thread::sleep;
use std::time::Duration;

use harness_tui::diff::diff_frames;
use harness_tui::terminal::{Terminal, install_panic_restore};
use harness_tui::text::{Color, Line, Span, Style};

fn main() {
    install_panic_restore();
    let mut term = match Terminal::stdout() {
        Ok(term) => term,
        Err(err) => {
            eprintln!("harness-tui smoke: {err}");
            return;
        }
    };
    let (width, height) = harness_tui::terminal::size().unwrap_or((80, 24));
    let origin = height.saturating_sub(3);
    let bold = Style {
        bold: true,
        ..Style::default()
    };
    let caret = Style {
        reverse: true,
        fg: Color::Ansi(6),
        ..Style::default()
    };
    let mut prev: Vec<Line> = Vec::new();
    for i in 0..30 {
        let next = vec![
            Line {
                spans: vec![Span::styled(
                    format!("harness-tui smoke on {width}x{height}"),
                    bold,
                )],
            },
            Line {
                spans: vec![Span::raw(format!("frame {i} ")), Span::styled(" ", caret)],
            },
        ];
        let updates = diff_frames(&prev, &next);
        term.present(&updates, origin).expect("present frame");
        prev = next;
        sleep(Duration::from_millis(100));
    }
}

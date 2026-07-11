//! Busy spinner with elapsed time — pure line builders; the app owns
//! the clock and the frame counter.

use crate::text::{Color, Line, Span, Style};

pub const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The two spinner rows: a blank spacer, then
/// ` ⠹ Working… (12s)` (spinner cyan, label dimmed).
pub fn spinner_lines(frame: usize, elapsed_secs: u64) -> Vec<Line> {
    let glyph = FRAMES[frame % FRAMES.len()];
    vec![
        Line::default(),
        Line {
            spans: vec![
                Span::styled(
                    format!(" {glyph} "),
                    Style {
                        fg: Color::Ansi(6),
                        ..Style::default()
                    },
                ),
                Span::styled(
                    format!("Working… ({})", format_elapsed(elapsed_secs)),
                    Style {
                        dim: true,
                        ..Style::default()
                    },
                ),
            ],
        },
    ]
}

/// `45s` under a minute, `2m 5s` beyond.
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

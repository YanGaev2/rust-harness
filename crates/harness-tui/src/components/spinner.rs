//! Busy spinner — pure line builders; the app owns the clock, the frame
//! counter and the detail text.

use crate::text::{Line, Span, Style};
use crate::theme::Theme;

pub const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The two spinner rows: a blank spacer, then
/// ` ⠹ Working… (12s · esc to interrupt)` — glyph in the accent color,
/// label and detail muted. An empty `detail` drops the parentheses.
pub fn spinner_lines(theme: &Theme, frame: usize, detail: &str) -> Vec<Line> {
    let glyph = FRAMES[frame % FRAMES.len()];
    let label = if detail.is_empty() {
        "Working…".to_string()
    } else {
        format!("Working… ({detail})")
    };
    vec![
        Line::default(),
        Line {
            spans: vec![
                Span::styled(format!(" {glyph} "), Style::fg(theme.accent)),
                Span::styled(label, Style::fg(theme.muted)),
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

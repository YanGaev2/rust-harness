//! Busy spinner — pure line builders; the app owns the clock, the frame
//! counter and the detail text.

use crate::text::{Color, Line, Span, Style};
use crate::theme::{Theme, mix};

pub const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Width, in characters, of the bright band that sweeps over the label.
const SHIMMER_BAND: f32 = 3.0;

/// The busy row ` ⠹ Working… (12s · esc to interrupt)`: glyph in the
/// accent color, detail muted, and a bright band sweeping over
/// `Working…`. `phase` (0..1) is the band position; the app derives it
/// from its clock. An empty `detail` drops the parentheses.
pub fn working_line(theme: &Theme, frame: usize, detail: &str, phase: f32) -> Line {
    let glyph = FRAMES[frame % FRAMES.len()];
    let mut spans = vec![Span::styled(format!(" {glyph} "), Style::fg(theme.accent))];
    spans.extend(shimmer("Working…", theme.muted, theme.shimmer, phase));
    if !detail.is_empty() {
        spans.push(Span::styled(format!(" ({detail})"), Style::fg(theme.muted)));
    }
    Line { spans }
}

/// `text` in the `base` color with a soft `highlight` band centered at
/// `phase` (0..1 sweeps from before the first character to after the
/// last). Without two RGB colors there is nothing to blend: plain `base`.
pub fn shimmer(text: &str, base: Color, highlight: Color, phase: f32) -> Vec<Span> {
    let (Color::Rgb(br, bg, bb), Color::Rgb(hr, hg, hb)) = (base, highlight) else {
        return vec![Span::styled(text, Style::fg(base))];
    };
    let count = text.chars().count() as f32;
    let center = phase.rem_euclid(1.0) * (count + 2.0 * SHIMMER_BAND) - SHIMMER_BAND;
    text.chars()
        .enumerate()
        .map(|(index, ch)| {
            let distance = (index as f32 - center).abs() / SHIMMER_BAND;
            let t = if distance < 1.0 {
                0.5 * (1.0 + (std::f32::consts::PI * distance).cos())
            } else {
                0.0
            };
            let (r, g, b) = mix((br, bg, bb), (hr, hg, hb), t);
            Span::styled(ch.to_string(), Style::fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

/// `45s` under a minute, `2m 5s` beyond.
pub fn format_elapsed(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

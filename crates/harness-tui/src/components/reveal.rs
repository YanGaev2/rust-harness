//! Paced reveal for streamed text.
//!
//! Network deltas arrive in bursts: nothing for 300 ms, then two hundred
//! characters at once. Text printed as it arrives jumps. [`Reveal`] shows
//! it at a steady pace instead. The pace follows the backlog, so the
//! display trails the stream by about [`LAG_SECS`] and never falls
//! further behind; a floor pace keeps the end of a stream from crawling.
//! [`fade_in`] colors the newest characters from the background toward
//! their own color, so new text appears softly instead of popping in.

use std::time::Instant;

use crate::text::{Color, Line, cells_to_line, line_cells};
use crate::theme::mix;

/// How far (seconds) the display trails the stream. Higher is smoother
/// and slower to catch up.
pub const LAG_SECS: f64 = 0.3;
/// Minimum pace, in characters per second, while a backlog is left.
pub const MIN_RATE: f64 = 90.0;
/// How many of the newest characters fade in.
pub const FADE_CHARS: usize = 12;
/// After the last character appears, the faded tail shrinks to nothing
/// over this time, so the end of a stream does not pop.
pub const FADE_OUT_SECS: f64 = 0.25;

/// How much of one growing text is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reveal {
    /// Bytes shown so far; always on a char boundary of the text.
    shown: usize,
    /// Fractional characters carried to the next frame, in 1/1000 char.
    carry_milli: u32,
    /// The previous `advance` call.
    last: Option<Instant>,
    /// When the shown prefix last grew.
    progressed: Option<Instant>,
}

impl Reveal {
    /// Nothing shown yet.
    pub fn new() -> Self {
        Reveal::default()
    }

    /// Bytes of the text shown so far.
    pub fn shown(&self) -> usize {
        self.shown
    }

    pub fn is_done(&self, text: &str) -> bool {
        self.shown >= text.len()
    }

    /// Show everything at once (history, interrupted animation).
    pub fn finish(&mut self, text: &str) {
        self.shown = text.len();
        self.carry_milli = 0;
        self.progressed = None;
    }

    /// Advance toward the end of `text` for the time since the previous
    /// call and return the visible prefix. The first call only starts
    /// the clock. `text` may grow between calls; it must not change.
    pub fn advance<'a>(&mut self, text: &'a str, now: Instant) -> &'a str {
        if self.shown > text.len() || !text.is_char_boundary(self.shown) {
            self.shown = text.len();
        }
        let elapsed = self.last.map_or(0.0, |last| {
            now.saturating_duration_since(last).as_secs_f64()
        });
        self.last = Some(now);
        let rest = &text[self.shown..];
        let backlog = rest.chars().count();
        if backlog == 0 {
            self.carry_milli = 0;
            return text;
        }
        let rate = (backlog as f64 / LAG_SECS).max(MIN_RATE);
        let budget = rate * elapsed + f64::from(self.carry_milli) / 1000.0;
        let step = (budget.floor() as usize).min(backlog);
        self.carry_milli = if step == backlog {
            0
        } else {
            ((budget - step as f64) * 1000.0) as u32
        };
        if step > 0 {
            self.shown += rest
                .char_indices()
                .nth(step)
                .map_or(rest.len(), |(index, _)| index);
            self.progressed = Some(now);
        }
        &text[..self.shown]
    }

    /// How many of the newest visible characters to fade at `now`: the
    /// full tail while text is still arriving, then a tail that shrinks
    /// to zero over [`FADE_OUT_SECS`].
    pub fn fade_len(&self, text: &str, now: Instant) -> usize {
        if !self.is_done(text) {
            return FADE_CHARS;
        }
        let Some(progressed) = self.progressed else {
            return 0;
        };
        let since = now.saturating_duration_since(progressed).as_secs_f64();
        let left = 1.0 - since / FADE_OUT_SECS;
        if left <= 0.0 {
            0
        } else {
            (FADE_CHARS as f64 * left).ceil() as usize
        }
    }

    /// True while the reveal still changes what is drawn.
    pub fn is_animating(&self, text: &str, now: Instant) -> bool {
        self.fade_len(text, now) > 0
    }
}

/// Fade in the newest `count` visible characters of `lines` (the rows of
/// a block being revealed): the newest is closest to `background`, older
/// ones step toward their own color. Spaces are skipped. `Color::Default`
/// counts as `foreground`; other non-RGB colors keep their color, since
/// their RGB value is unknown.
pub fn fade_in(
    lines: &mut [Line],
    count: usize,
    background: (u8, u8, u8),
    foreground: (u8, u8, u8),
) {
    let mut faded = 0usize;
    for line in lines.iter_mut().rev() {
        if faded >= count {
            break;
        }
        let rebuilt = {
            let mut cells = line_cells(line);
            for cell in cells.iter_mut().rev() {
                if faded >= count {
                    break;
                }
                if cell.grapheme.trim().is_empty() {
                    continue;
                }
                let base = match cell.style.fg {
                    Color::Rgb(r, g, b) => Some((r, g, b)),
                    Color::Default => Some(foreground),
                    Color::Ansi(_) | Color::Indexed(_) => None,
                };
                if let Some(base) = base {
                    let t = (faded + 1) as f32 / (count + 1) as f32;
                    let (r, g, b) = mix(background, base, t);
                    cell.style.fg = Color::Rgb(r, g, b);
                }
                faded += 1;
            }
            cells_to_line(cells)
        };
        *line = rebuilt;
    }
}

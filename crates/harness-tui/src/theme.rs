//! Color tokens shared by the components and the front ends.
//!
//! Every gray is an explicit color, never the terminal's `dim` or
//! `italic` attribute: terminals and fonts render those differently (dim
//! may be ignored, italic may turn cursive), an explicit RGB value looks
//! the same everywhere. Body text keeps the terminal's default
//! foreground so the user's own color scheme still owns the main text.

use crate::text::Color;

/// The palette. Values are 24-bit; [`Theme::quantized`] maps them onto
/// the 256-color palette for terminals without truecolor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Prompt glyph, spinner, list markers, links.
    pub accent: Color,
    /// Markdown headings.
    pub heading: Color,
    /// Secondary text: thinking, tool arguments, footer, placeholder.
    pub muted: Color,
    /// Rules and code fences — structure that should recede.
    pub faint: Color,
    /// Inline code.
    pub code: Color,
    /// Background strip behind the user's own messages.
    pub user_bg: Color,
    pub success: Color,
    pub error: Color,
    pub warning: Color,
    /// Diff rows: background of added/removed lines and their sign color.
    pub diff_add_bg: Color,
    pub diff_del_bg: Color,
    pub diff_add: Color,
    pub diff_del: Color,
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            accent: Color::Rgb(0x8a, 0xbe, 0xb7),
            heading: Color::Rgb(0xf0, 0xc6, 0x74),
            muted: Color::Rgb(0x80, 0x80, 0x80),
            faint: Color::Rgb(0x50, 0x50, 0x50),
            code: Color::Rgb(0xa5, 0xb4, 0xfc),
            user_bg: Color::Rgb(0x2a, 0x2d, 0x37),
            success: Color::Rgb(0xb5, 0xbd, 0x68),
            error: Color::Rgb(0xcc, 0x66, 0x66),
            warning: Color::Rgb(0xf5, 0xa7, 0x42),
            diff_add_bg: Color::Rgb(0x1f, 0x33, 0x26),
            diff_del_bg: Color::Rgb(0x3a, 0x22, 0x28),
            diff_add: Color::Rgb(0xb8, 0xdb, 0x87),
            diff_del: Color::Rgb(0xe2, 0x6a, 0x75),
        }
    }

    pub fn light() -> Theme {
        Theme {
            accent: Color::Rgb(0x2f, 0x7d, 0x78),
            heading: Color::Rgb(0x9a, 0x6b, 0x10),
            muted: Color::Rgb(0x6c, 0x6c, 0x6c),
            faint: Color::Rgb(0xb0, 0xb0, 0xb0),
            code: Color::Rgb(0x4f, 0x46, 0xe5),
            user_bg: Color::Rgb(0xe8, 0xe8, 0xec),
            success: Color::Rgb(0x58, 0x84, 0x58),
            error: Color::Rgb(0xaa, 0x44, 0x44),
            warning: Color::Rgb(0xb0, 0x70, 0x0c),
            diff_add_bg: Color::Rgb(0xdc, 0xef, 0xdc),
            diff_del_bg: Color::Rgb(0xf7, 0xd8, 0xdb),
            diff_add: Color::Rgb(0x2e, 0x7d, 0x32),
            diff_del: Color::Rgb(0xc0, 0x39, 0x2b),
        }
    }

    /// Pick the variant for a terminal background color (as reported by
    /// an OSC 11 query); unknown backgrounds get the dark palette.
    pub fn for_background(background: Option<(u8, u8, u8)>) -> Theme {
        match background {
            Some((r, g, b)) if relative_luminance(r, g, b) > 0.5 => Theme::light(),
            _ => Theme::dark(),
        }
    }

    /// The same palette mapped onto the xterm 256-color cube and gray
    /// ramp, for terminals that cannot render 24-bit color.
    pub fn quantized(self) -> Theme {
        let q = quantize;
        Theme {
            accent: q(self.accent),
            heading: q(self.heading),
            muted: q(self.muted),
            faint: q(self.faint),
            code: q(self.code),
            user_bg: q(self.user_bg),
            success: q(self.success),
            error: q(self.error),
            warning: q(self.warning),
            diff_add_bg: q(self.diff_add_bg),
            diff_del_bg: q(self.diff_del_bg),
            diff_add: q(self.diff_add),
            diff_del: q(self.diff_del),
        }
    }
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::dark()
    }
}

/// Perceived brightness in 0..=1 (Rec. 709 weights on sRGB values; the
/// light/dark split does not need gamma-exact luminance).
fn relative_luminance(r: u8, g: u8, b: u8) -> f32 {
    (0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32) / 255.0
}

/// Nearest xterm-256 entry for an RGB color; other colors pass through.
pub fn quantize(color: Color) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |v: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] as i32 - v as i32).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    // Gray ramp 232..=255 covers 8, 18, …, 238.
    let average = (r as u32 + g as u32 + b as u32) / 3;
    let gray_step = ((average.saturating_sub(8) + 5) / 10).min(23);
    let gray = (8 + 10 * gray_step) as u8;
    let distance = |(cr, cg, cb): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(r, cr) + d(g, cg) + d(b, cb)
    };
    if distance((gray, gray, gray)) < distance(cube) {
        Color::Indexed(232 + gray_step as u8)
    } else {
        Color::Indexed(cube_index as u8)
    }
}

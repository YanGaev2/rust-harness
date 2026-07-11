//! One-row status line: left content, right-aligned hints that are
//! dropped whole when they don't fit.

use crate::text::{Line, Span};

pub fn status_line(width: usize, left: Line, right: Line) -> Line {
    let left_width = left.width();
    let right_width = right.width();
    if right_width == 0 || left_width + right_width + 1 > width {
        return left;
    }
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(width - left_width - right_width)));
    spans.extend(right.spans);
    Line { spans }
}

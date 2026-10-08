//! One-row status line: left content, right-aligned content that is
//! dropped whole when it does not fit; a left side wider than the row is
//! clipped with an ellipsis instead of wrapping.

use crate::text::{Line, Span, truncate};

pub fn status_line(width: usize, left: Line, right: Line) -> Line {
    let left_width = left.width();
    let right_width = right.width();
    if right_width == 0 || left_width + right_width + 2 > width {
        return truncate(&left, width);
    }
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(width - left_width - right_width)));
    spans.extend(right.spans);
    Line { spans }
}

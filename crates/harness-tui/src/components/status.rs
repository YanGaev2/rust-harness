//! One-row status line: left content, right-aligned content that is
//! dropped whole when it does not fit; a left side wider than the row is
//! clipped with an ellipsis instead of wrapping.

use crate::text::{Line, Span, visible_width};

pub fn status_line(width: usize, left: Line, right: Line) -> Line {
    let left_width = left.width();
    let right_width = right.width();
    if right_width == 0 || left_width + right_width + 2 > width {
        return clip_with_ellipsis(left, width);
    }
    let mut spans = left.spans;
    spans.push(Span::raw(" ".repeat(width - left_width - right_width)));
    spans.extend(right.spans);
    Line { spans }
}

/// Clip a line to `width` columns, ending in `…` when anything was cut.
fn clip_with_ellipsis(line: Line, width: usize) -> Line {
    if line.width() <= width {
        return line;
    }
    let budget = width.saturating_sub(1);
    let mut used = 0usize;
    let mut spans = Vec::new();
    for span in line.spans {
        let mut text = String::new();
        for ch in span.text.chars() {
            let mut buf = [0u8; 4];
            let w = visible_width(ch.encode_utf8(&mut buf));
            if used + w > budget {
                break;
            }
            used += w;
            text.push(ch);
        }
        let truncated = text.len() < span.text.len();
        spans.push(Span::styled(text, span.style));
        if truncated {
            if let Some(last) = spans.last_mut() {
                last.text.push('…');
            }
            break;
        }
    }
    Line { spans }
}

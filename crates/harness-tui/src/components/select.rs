//! Simple selection list (used by the setup wizard).

use crate::text::{Line, Span, Style};

/// `> ` marks the selected item; others are indented to align.
pub fn select_lines(items: &[&str], selected: usize) -> Vec<Line> {
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            if i == selected {
                Line {
                    spans: vec![
                        Span::styled(
                            "> ",
                            Style {
                                bold: true,
                                ..Style::default()
                            },
                        ),
                        Span::raw(*item),
                    ],
                }
            } else {
                Line::raw(format!("  {item}"))
            }
        })
        .collect()
}

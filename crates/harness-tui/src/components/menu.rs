//! Slash-command completion menu, rendered below the editor.

use crate::text::{Line, Span, Style};

pub struct MenuItem {
    pub name: String,
    pub usage: String,
    pub description: String,
}

/// Menu rows: ` ▸ ` marks the selected row, the typed prefix is bold,
/// the usage column is padded to a fixed width, the description dimmed.
pub fn menu_lines(items: &[MenuItem], query: &str, selected: usize, max_rows: usize) -> Vec<Line> {
    const USAGE_COLUMN: usize = 26;
    let dim = Style {
        dim: true,
        ..Style::default()
    };
    items
        .iter()
        .take(max_rows.max(1))
        .enumerate()
        .map(|(i, item)| {
            let marker = if i == selected { " \u{25b8} " } else { "   " };
            let typed = query.len().min(item.name.len());
            let usage_tail = item.usage.get(item.name.len()..).unwrap_or("");
            let pad = USAGE_COLUMN.saturating_sub(item.usage.len()) + 1;
            Line {
                spans: vec![
                    Span::raw(marker),
                    Span::styled(
                        item.name[..typed].to_string(),
                        Style {
                            bold: true,
                            ..Style::default()
                        },
                    ),
                    Span::raw(item.name[typed..].to_string()),
                    Span::styled(format!("{usage_tail}{}", " ".repeat(pad)), dim),
                    Span::styled(item.description.clone(), dim),
                ],
            }
        })
        .collect()
}

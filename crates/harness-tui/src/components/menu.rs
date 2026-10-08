//! Slash-command completion menu, rendered below the editor.

use crate::text::{Line, Span, Style};
use crate::theme::Theme;

pub struct MenuItem {
    pub name: String,
    pub usage: String,
    pub description: String,
}

/// Menu rows: ` › ` and the accent color mark the selected row, the typed
/// prefix is bold, the usage column is padded to a fixed width, usage
/// tail and description are muted.
pub fn menu_lines(
    theme: &Theme,
    items: &[MenuItem],
    query: &str,
    selected: usize,
    max_rows: usize,
) -> Vec<Line> {
    const USAGE_COLUMN: usize = 26;
    let muted = Style::fg(theme.muted);
    items
        .iter()
        .take(max_rows.max(1))
        .enumerate()
        .map(|(i, item)| {
            let is_selected = i == selected;
            let marker = if is_selected { " \u{203a} " } else { "   " };
            let name_style = if is_selected {
                Style::fg(theme.accent)
            } else {
                Style::default()
            };
            let typed = query.len().min(item.name.len());
            let usage_tail = item.usage.get(item.name.len()..).unwrap_or("");
            let pad = USAGE_COLUMN.saturating_sub(item.usage.len()) + 1;
            Line {
                spans: vec![
                    Span::styled(marker, Style::fg(theme.accent)),
                    Span::styled(item.name[..typed].to_string(), name_style.bold()),
                    Span::styled(item.name[typed..].to_string(), name_style),
                    Span::styled(format!("{usage_tail}{}", " ".repeat(pad)), muted),
                    Span::styled(item.description.clone(), muted),
                ],
            }
        })
        .collect()
}

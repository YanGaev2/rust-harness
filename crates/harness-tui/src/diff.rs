//! Frame diffing: compute the minimal row updates between two frames.

use crate::text::Line;

/// One write the terminal must perform to turn the previous frame into
/// the next one. Rows are 0-based, relative to the frame origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowUpdate {
    Write { row: usize, line: Line },
    Clear { row: usize },
}

/// Compare frames row by row. Unchanged rows produce no updates; rows
/// that exist only in `prev` are cleared. Passing `prev = &[]` forces a
/// full redraw (used after resize).
pub fn diff_frames(prev: &[Line], next: &[Line]) -> Vec<RowUpdate> {
    let mut updates = Vec::new();
    for (row, line) in next.iter().enumerate() {
        if prev.get(row) != Some(line) {
            updates.push(RowUpdate::Write {
                row,
                line: line.clone(),
            });
        }
    }
    for row in next.len()..prev.len() {
        updates.push(RowUpdate::Clear { row });
    }
    updates
}

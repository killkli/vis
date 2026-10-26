//! Display-cell wrapping shared by the renderer and viewport motions.
//!
//! The C view lays source bytes out into display rows: tabs expand one cell at
//! a time, wide and control tokens stay together, a newline occupies one cell
//! before the row advances, and EOF adds a final cursor cell. Rendering and
//! screen-line motions must agree on that layout, so both use this module.
//! Nothing proportional to the file size is stored; callers stream cells or
//! stop early.
use std::ops::Range;

use crate::motion;

/// Measured geometry of a focused window's body, as last laid out by the UI.
/// The editor keeps it so viewport motions use the real screen size and the
/// scroll position the user is looking at.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ViewportMetrics {
    /// Body columns (the full window width).
    pub width: usize,
    /// Body rows, excluding the status line.
    pub height: usize,
    /// First displayed soft-wrapped row.
    pub top: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CellKind {
    Source,
    Space,
    Replacement,
    Eof,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WrappedCell {
    /// Zero-based display row.
    pub row: usize,
    /// One-based source line displayed in this row.
    pub line: usize,
    /// Zero-based display column of the cell's left edge.
    pub column: usize,
    /// Display width in columns; continuation cells are separate entries only
    /// in the renderer's grid, not in this stream.
    pub width: usize,
    /// Byte position in the buffer.
    pub pos: usize,
    /// Source bytes consumed by this cell.
    pub len: usize,
    pub kind: CellKind,
}

fn token_metrics(data: &[u8], pos: usize, column: usize) -> (usize, usize) {
    let byte = data[pos];
    if byte < 32 || byte == 127 {
        return (2, 1);
    }
    if byte < 128 {
        return (1, 1);
    }
    let len = match byte {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    let Some(text) = data
        .get(pos..pos.saturating_add(len))
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
    else {
        return (4, 1);
    };
    if let Some(character) = text.chars().next().filter(|c| c.is_control()) {
        return (format!("\\u{{{:x}}}", character as u32).len(), len);
    }
    let width = motion::codepoint_width(data, pos);
    (if width == 0 && column == 0 { 1 } else { width }, len)
}

/// Stream the C view's default cell wrapping (empty breakat). Returning `false`
/// from `visit` stops the walk.
pub fn walk_cells(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    mut visit: impl FnMut(WrappedCell) -> bool,
) {
    let tab_width = tab_width.clamp(1, 8);
    let mut row = 0;
    let mut column = 0;
    let mut pos = 0;
    let mut line = 1;
    while pos < data.len() {
        let byte = data[pos];
        if byte == b'\t' {
            let count = tab_width - column % tab_width;
            for _ in 0..count {
                if column == columns {
                    row += 1;
                    column = 0;
                }
                if !visit(WrappedCell {
                    row,
                    line,
                    column,
                    width: 1,
                    pos,
                    len: 1,
                    kind: CellKind::Space,
                }) {
                    return;
                }
                column += 1;
            }
            pos += 1;
        } else if byte == b'\n' {
            if column == columns {
                row += 1;
                column = 0;
            }
            if !visit(WrappedCell {
                row,
                line,
                column,
                width: 1,
                pos,
                len: 1,
                kind: CellKind::Space,
            }) {
                return;
            }
            row += 1;
            column = 0;
            pos += 1;
            line += 1;
        } else {
            let (mut width, len) = token_metrics(data, pos, column);
            let kind = if width > columns {
                width = 1;
                CellKind::Replacement
            } else {
                CellKind::Source
            };
            if column + width > columns {
                row += 1;
                column = 0;
            }
            if !visit(WrappedCell {
                row,
                line,
                column,
                width,
                pos,
                len,
                kind,
            }) {
                return;
            }
            column += width;
            pos += len;
        }
    }
    if column == columns {
        row += 1;
        column = 0;
    }
    let _ = visit(WrappedCell {
        row,
        line,
        column,
        width: 1,
        pos,
        len: 0,
        kind: CellKind::Eof,
    });
}

/// The source line displayed in a row.
pub fn line_number_at_row(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
) -> Option<usize> {
    let mut line = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.row == row {
            line = Some(cell.line);
            return false;
        }
        true
    });
    line
}

/// The source line numbers of the display rows in a range. The native line
/// number sidebar uses them to label the visible rows.
pub fn row_line_numbers(
    data: &[u8],
    rows: Range<usize>,
    columns: usize,
    tab_width: usize,
) -> Vec<usize> {
    let mut numbers = Vec::new();
    let mut last: Option<usize> = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.row < rows.start {
            return true;
        }
        if cell.row >= rows.end {
            return false;
        }
        if last != Some(cell.row) {
            last = Some(cell.row);
            numbers.push(cell.line);
        }
        true
    });
    numbers
}

/// Zero-based default soft-wrapped screen coordinates for a byte position.
/// Width zero is treated as one.
pub fn screen_position(
    data: &[u8],
    position: usize,
    columns: usize,
    tab_width: usize,
) -> (usize, usize) {
    let position = position.min(data.len());
    let mut coordinate = (0, 0);
    walk_cells(data, columns.max(1), tab_width, |cell| {
        let found = if position == data.len() {
            cell.kind == CellKind::Eof
        } else {
            cell.pos <= position && position < cell.pos + cell.len
        };
        if found {
            coordinate = (cell.row, cell.column);
        }
        !found
    });
    coordinate
}

/// First source byte of a display row, or `None` when the row is not reached
/// before EOF. C positions window motions at the start of the requested row.
pub fn row_start(data: &[u8], row: usize, columns: usize, tab_width: usize) -> Option<usize> {
    let mut start = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.kind == CellKind::Eof {
            return false;
        }
        if cell.row == row {
            start = Some(cell.pos);
            return false;
        }
        cell.row < row
    });
    start
}

/// Last source byte of a display row's final cell, or `None` outside the text.
pub fn row_finish(data: &[u8], row: usize, columns: usize, tab_width: usize) -> Option<usize> {
    let mut finish = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.kind == CellKind::Eof {
            return false;
        }
        if cell.row == row {
            finish = Some(cell.pos);
            return true;
        }
        cell.row < row
    });
    finish
}

/// Number of display columns occupied by a row, or `None` past the text.
pub fn row_width(data: &[u8], row: usize, columns: usize, tab_width: usize) -> Option<usize> {
    row_end_column(data, row, columns, tab_width).map(|end| end + 1)
}

/// Index of the final display row, which holds the EOF cursor cell.
pub fn last_row(data: &[u8], columns: usize, tab_width: usize) -> usize {
    let mut last = 0;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        last = cell.row;
        true
    });
    last
}

/// Index of the last row holding source cells. Native window motions clamp
/// here rather than to the virtual EOF cursor row, so `L` in a short file lands
/// on the last real line as C does.
pub fn last_source_row(data: &[u8], columns: usize, tab_width: usize) -> Option<usize> {
    let mut last = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.kind != CellKind::Eof {
            last = Some(cell.row);
        }
        true
    });
    last
}

/// Display position of the last cursor cell in a row: C's screen-line end uses
/// `line.width - 1`, i.e. the start column of the final cell.
pub fn row_end_column(data: &[u8], row: usize, columns: usize, tab_width: usize) -> Option<usize> {
    let columns = columns.max(1);
    let mut end = None;
    walk_cells(data, columns, tab_width, |cell| {
        if cell.kind == CellKind::Eof {
            return false;
        }
        if cell.row == row {
            end = Some(if cell.width > 0 {
                cell.column + cell.width - 1
            } else {
                cell.column
            });
            return true;
        }
        cell.row < row
    });
    end
}

/// Byte position at a display coordinate, moving left to the cell that starts a
/// wide token, as C's `cursor_set` does.
pub fn position_at(
    data: &[u8],
    row: usize,
    column: usize,
    columns: usize,
    tab_width: usize,
) -> Option<usize> {
    let mut position = None;
    walk_cells(data, columns.max(1), tab_width, |cell| {
        if cell.kind == CellKind::Eof {
            return false;
        }
        if cell.row > row {
            return false;
        }
        if cell.row == row {
            if cell.column <= column && column < cell.column + cell.width {
                position = Some(cell.pos);
                return false;
            }
            position = Some(cell.pos);
        }
        true
    });
    position
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_wrapping_matches_the_renderer_corpus() {
        assert_eq!(screen_position(b"abcde\nX", 5, 5, 8), (1, 0));
        assert_eq!(screen_position(b"abcde\nX", 6, 5, 8), (2, 0));
        assert_eq!(screen_position(b"abcdefghij", 10, 5, 8), (2, 0));
        assert_eq!(screen_position(b"a\tb", 2, 5, 8), (1, 3));
        assert_eq!(screen_position(b"a\tb", 2, 8, 4), (0, 4));
        let data = "ab界c".as_bytes();
        assert_eq!(screen_position(data, 2, 3, 8), (1, 0));
        assert_eq!(screen_position(data, 5, 3, 8), (1, 2));
        assert_eq!(screen_position(data, 6, 3, 8), (2, 0));
    }

    #[test]
    fn rows_expose_start_finish_end_and_positions() {
        let data = b"abcde\nX";
        assert_eq!(row_start(data, 0, 5, 8), Some(0));
        assert_eq!(row_start(data, 1, 5, 8), Some(5));
        assert_eq!(row_start(data, 2, 5, 8), Some(6));
        assert_eq!(row_start(data, 3, 5, 8), None);
        assert_eq!(row_finish(data, 1, 5, 8), Some(5));
        assert_eq!(row_finish(data, 2, 5, 8), Some(6));
        assert_eq!(row_end_column(data, 0, 5, 8), Some(4));
        assert_eq!(row_end_column(data, 1, 5, 8), Some(0));
        assert_eq!(position_at(data, 0, 3, 5, 8), Some(3));
        assert_eq!(position_at(data, 1, 0, 5, 8), Some(5));
        let wide = "ab界c".as_bytes();
        assert_eq!(position_at(wide, 1, 1, 3, 8), Some(2));
        assert_eq!(position_at(wide, 1, 2, 3, 8), Some(5));
        assert_eq!(row_end_column(wide, 1, 3, 8), Some(2));
    }
}

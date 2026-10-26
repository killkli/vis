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
    /// The first cell of an expanded tab, which carries the tab symbol.
    Tab,
    /// The remaining cells of an expanded tab.
    TabFill,
    /// The cell of a line ending.
    Newline,
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

/// One cell of a logical line, before the wrapping is decided.
struct LineCell {
    pos: usize,
    len: usize,
    kind: CellKind,
    width: usize,
    /// The column the cell takes on its row.
    column: usize,
    /// The cell is a break character, so a word may wrap after it.
    brk: bool,
}

/// A token of a logical line.
enum Token {
    Text { pos: usize },
    Tab { pos: usize },
    Newline { pos: usize },
}

/// The length of the UTF-8 sequence starting with the given byte.
fn utf8_length(byte: u8) -> usize {
    match byte {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    }
}

/// Stream the C view's default cell wrapping (empty breakat). Returning `false`
/// from `visit` stops the walk.
pub fn walk_cells(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    visit: impl FnMut(WrappedCell) -> bool,
) {
    walk_cells_with_breakat(data, columns, tab_width, "", visit);
}

/// Stream the C view's cell wrapping. `breakat` names the characters that may
/// end a line, like the native `view->breakat`; an empty string wraps at the
/// window width like the native default. Returning `false` from `visit` stops
/// the walk.
pub fn walk_cells_with_breakat(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    breakat: &str,
    mut visit: impl FnMut(WrappedCell) -> bool,
) {
    let tab_width = tab_width.clamp(1, 8);
    let columns = columns.max(1);
    let mut row = 0;
    let mut line = 1;
    let mut pos = 0;
    let mut tokens: Vec<Token> = Vec::new();
    while pos < data.len() {
        let byte = data[pos];
        if byte == b'\n' {
            tokens.push(Token::Newline { pos });
            let (rows, _, stopped) = place_line(
                &tokens, row, line, columns, tab_width, breakat, data, &mut visit,
            );
            if stopped {
                return;
            }
            row += rows;
            tokens.clear();
            line += 1;
            pos += 1;
        } else if byte == b'\t' {
            tokens.push(Token::Tab { pos });
            pos += 1;
        } else {
            let len = utf8_length(byte);
            tokens.push(Token::Text { pos });
            pos += len;
        }
    }
    let mut eof_column = 0;
    if !tokens.is_empty() {
        let (rows, column, stopped) = place_line(
            &tokens, row, line, columns, tab_width, breakat, data, &mut visit,
        );
        if stopped {
            return;
        }
        // The end of file cursor sits behind the last cell of the line.
        if column >= columns {
            row += rows;
            eof_column = 0;
        } else {
            row += rows.saturating_sub(1);
            eof_column = column;
        }
    }
    let _ = visit(WrappedCell {
        row,
        line,
        column: eof_column,
        width: 1,
        pos: data.len(),
        len: 0,
        kind: CellKind::Eof,
    });
}

/// Place the cells of one logical line, wrapping like the native view, and
/// return the number of rows the line occupies and the column behind its
/// last cell.
#[allow(clippy::too_many_arguments)]
fn place_line(
    tokens: &[Token],
    base_row: usize,
    line: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
    data: &[u8],
    visit: &mut impl FnMut(WrappedCell) -> bool,
) -> (usize, usize, bool) {
    // The cells placed on the current row.
    let mut row_cells: Vec<LineCell> = Vec::new();
    let mut out_row = 0;
    let mut out_column = 0;
    // The column after the last break character, where a word may wrap.
    let mut wrap_column = 0;
    // Whether the previously placed cell was a break character.
    let mut previous_break = false;
    let mut pending: Vec<LineCell> = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if pending.is_empty() {
            pending = match &tokens[index] {
                Token::Text { pos } => {
                    let (width, len) = token_metrics(data, *pos, out_column);
                    let kind = if width > columns {
                        CellKind::Replacement
                    } else if data[*pos] == b' ' {
                        // Native `view_expand_space` draws the space symbol.
                        CellKind::Space
                    } else {
                        CellKind::Source
                    };
                    let width = if kind == CellKind::Replacement {
                        1
                    } else {
                        width
                    };
                    let text = data
                        .get(*pos..*pos + len)
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .unwrap_or("");
                    vec![LineCell {
                        pos: *pos,
                        len,
                        kind,
                        width,
                        column: 0,
                        brk: !breakat.is_empty() && breakat.contains(text),
                    }]
                }
                Token::Tab { pos } => {
                    let count = tab_width - out_column % tab_width;
                    (0..count)
                        .map(|index| LineCell {
                            pos: *pos,
                            len: 1,
                            kind: if index == 0 {
                                CellKind::Tab
                            } else {
                                CellKind::TabFill
                            },
                            width: 1,
                            column: 0,
                            brk: false,
                        })
                        .collect()
                }
                Token::Newline { pos } => vec![LineCell {
                    pos: *pos,
                    len: 1,
                    kind: CellKind::Newline,
                    width: 1,
                    column: 0,
                    brk: false,
                }],
            };
        }
        let mut cell = pending.remove(0);
        if previous_break && !cell.brk {
            // The native view records the column after a break character.
            wrap_column = out_column;
        }
        previous_break = cell.brk;
        if out_column + cell.width > columns && !row_cells.is_empty() {
            // The native view wraps at the last break character and moves the
            // rest of the row to the next one.
            let split = if wrap_column > 0 {
                row_cells
                    .iter()
                    .position(|placed| placed.column >= wrap_column)
                    .filter(|split| *split > 0)
            } else {
                None
            };
            match split {
                Some(split) => {
                    let moved: Vec<LineCell> = row_cells.drain(split..).collect();
                    if !emit_row(&row_cells, base_row + out_row, line, visit) {
                        return (out_row, out_column, true);
                    }
                    out_row += 1;
                    out_column = 0;
                    wrap_column = 0;
                    for mut moved_cell in moved {
                        moved_cell.column = out_column;
                        out_column += moved_cell.width;
                        row_cells.push(moved_cell);
                    }
                    // The cell that triggered the wrap starts the new row.
                    pending.insert(0, cell);
                    continue;
                }
                None => {
                    if !emit_row(&row_cells, base_row + out_row, line, visit) {
                        return (out_row, out_column, true);
                    }
                    out_row += 1;
                    out_column = 0;
                    row_cells.clear();
                    wrap_column = 0;
                    // The cell that triggered the wrap starts the new row.
                    pending.insert(0, cell);
                    continue;
                }
            }
        }
        cell.column = out_column;
        out_column += cell.width;
        row_cells.push(cell);
        if pending.is_empty() {
            index += 1;
        }
    }
    if !emit_row(&row_cells, base_row + out_row, line, visit) {
        return (out_row, out_column, true);
    }
    (out_row + 1, out_column, false)
}

/// Emit the cells of one row.
fn emit_row(
    row_cells: &[LineCell],
    row: usize,
    line: usize,
    visit: &mut impl FnMut(WrappedCell) -> bool,
) -> bool {
    for cell in row_cells {
        if !visit(WrappedCell {
            row,
            line,
            column: cell.column,
            width: cell.width,
            pos: cell.pos,
            len: cell.len,
            kind: cell.kind,
        }) {
            return false;
        }
    }
    true
}

/// The source line displayed in a row.
pub fn line_number_at_row(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
) -> Option<usize> {
    line_number_at_row_with_breakat(data, row, columns, tab_width, "")
}

pub fn line_number_at_row_with_breakat(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut line = None;
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    row_line_numbers_with_breakat(data, rows, columns, tab_width, "")
}

pub fn row_line_numbers_with_breakat(
    data: &[u8],
    rows: Range<usize>,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Vec<usize> {
    let mut numbers = Vec::new();
    let mut last: Option<usize> = None;
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    screen_position_with_breakat(data, position, columns, tab_width, "")
}

pub fn screen_position_with_breakat(
    data: &[u8],
    position: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> (usize, usize) {
    let position = position.min(data.len());
    let mut coordinate = (0, 0);
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    row_start_with_breakat(data, row, columns, tab_width, "")
}

pub fn row_start_with_breakat(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut start = None;
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    row_finish_with_breakat(data, row, columns, tab_width, "")
}

pub fn row_finish_with_breakat(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut finish = None;
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    row_width_with_breakat(data, row, columns, tab_width, "")
}

pub fn row_width_with_breakat(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    row_end_column_with_breakat(data, row, columns, tab_width, breakat).map(|end| end + 1)
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
    row_end_column_with_breakat(data, row, columns, tab_width, "")
}

pub fn row_end_column_with_breakat(
    data: &[u8],
    row: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let columns = columns.max(1);
    let mut end = None;
    walk_cells_with_breakat(data, columns, tab_width, breakat, |cell| {
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
    position_at_with_breakat(data, row, column, columns, tab_width, "")
}

pub fn position_at_with_breakat(
    data: &[u8],
    row: usize,
    column: usize,
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut position = None;
    walk_cells_with_breakat(data, columns.max(1), tab_width, breakat, |cell| {
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
    /// The number of columns every row occupies.
    fn row_widths(data: &[u8], columns: usize, breakat: &str) -> Vec<usize> {
        let mut widths: Vec<usize> = Vec::new();
        walk_cells_with_breakat(data, columns, 8, breakat, |cell| {
            while widths.len() <= cell.row {
                widths.push(0);
            }
            widths[cell.row] = widths[cell.row].max(cell.column + cell.width);
            true
        });
        widths
    }

    #[test]
    fn the_break_characters_keep_a_word_together() {
        let data = b"hello world foo\n";
        // Without break characters the text wraps at the column.
        // Without break characters the text wraps at the column.
        assert_eq!(&row_widths(data, 8, "")[..2], [8, 8]);
        // A space as break character moves the word to the next row, so the
        // first row holds only "hello ".
        assert_eq!(&row_widths(data, 8, " ")[..2], [6, 6]);
    }

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

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

mod native;
pub(crate) use native::NativeView;

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
    /// Native per-window wrapcolumn option; zero means the full body width.
    pub wrapcolumn: usize,
}

impl ViewportMetrics {
    pub fn wrap_width(self) -> usize {
        let width = self.width.max(1);
        if self.wrapcolumn > 0 {
            width.min(self.wrapcolumn).max(1)
        } else {
            width
        }
    }
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
    /// The byte after the characters that are not wide and join this cell.
    ///
    /// The original editor merges a character that is not wide into the
    /// character before it, so the cell holds both. Zero means the cell
    /// holds no other character.
    pub merged_end: usize,
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
#[derive(Clone)]
struct LineCell {
    pos: usize,
    len: usize,
    kind: CellKind,
    width: usize,
    /// The column the cell takes on its row.
    column: usize,
    /// The cell is a break character, so a word may wrap after it.
    brk: bool,
    /// The byte after the characters that are not wide and join this cell.
    ///
    /// The original editor merges a character that is not wide into the
    /// character before it, so the cell holds both. Zero means the cell
    /// holds no other character.
    merged_end: usize,
}

enum Token {
    Text { pos: usize },
    Tab { pos: usize },
    Newline { pos: usize },
    Eof { pos: usize },
}

/// The length of the UTF-8 sequence starting with the given byte.
pub fn utf8_length(byte: u8) -> usize {
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

/// Stream the C view's cell wrapping. `breakat` names characters which may
/// end a line; `columns` is both the view width and wrap limit.
pub fn walk_cells_with_breakat(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    breakat: &str,
    visit: impl FnMut(WrappedCell) -> bool,
) {
    walk_cells_with_layout(data, columns, columns, tab_width, breakat, visit);
}

/// Stream cells with separate physical viewport width and text wrap width.
/// C draws the EOF cursor within the physical view even when `wrapcolumn`
/// limits source text to a narrower width.
pub fn walk_cells_with_layout(
    data: &[u8],
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
    visit: impl FnMut(WrappedCell) -> bool,
) {
    walk_cells_from_layout(
        data,
        0,
        0,
        1,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
        visit,
    );
}

/// Stream the cells of the text from a byte.
///
/// The original editor reads the text from the start of its view, so a view
/// that begins in the middle of a long line shows the line from there. The
/// rows the walk reports are the rows of the view, counted from the row the
/// start byte is on.
#[allow(clippy::too_many_arguments)]
pub fn walk_cells_from_layout(
    data: &[u8],
    start: usize,
    start_row: usize,
    start_line: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
    visit: impl FnMut(WrappedCell) -> bool,
) {
    walk_cells_with_row_end(
        data,
        start,
        start_row,
        start_line,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
        visit,
        |_, _| {},
    );
}

/// The native read offset can exceed the bytes retained on a row when word
/// wrapping moves a suffix off the bottom of a view. Keep it separately from
/// the cell byte counts used by cursor_set.
#[allow(clippy::too_many_arguments)]
pub(crate) fn walk_cells_with_row_end(
    data: &[u8],
    start: usize,
    start_row: usize,
    start_line: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
    mut visit: impl FnMut(WrappedCell) -> bool,
    mut row_end: impl FnMut(usize, usize),
) {
    let tab_width = tab_width.clamp(1, 8);
    let columns = wrap_columns.max(1);
    let eof_columns = physical_columns.max(1);
    let mut row = start_row;
    let mut line = start_line;
    let mut pos = start.min(data.len());
    loop {
        let at_eof = pos >= data.len();
        // Decode only as far as the consumer needs. A logical line can span
        // megabytes while a view needs just a few wrapped rows of it.
        let mut finished = false;
        let tokens = std::iter::from_fn(|| {
            if finished {
                return None;
            }
            let token_pos = pos;
            if pos >= data.len() {
                finished = true;
                return Some(Token::Eof { pos: data.len() });
            }
            let token = match data[pos] {
                b'\n' => {
                    finished = true;
                    Token::Newline { pos }
                }
                b'\t' => Token::Tab { pos },
                _ => Token::Text { pos },
            };
            pos = token_pos
                + if matches!(token, Token::Text { .. }) {
                    utf8_length(data[token_pos])
                } else {
                    1
                };
            Some(token)
        });
        let (rows, _, stopped) = place_line(
            tokens,
            row,
            line,
            columns,
            eof_columns,
            tab_width,
            breakat,
            data,
            &mut visit,
            &mut row_end,
        );
        if stopped || at_eof || pos >= data.len() && data.last() != Some(&b'\n') {
            return;
        }
        row += rows;
        line += 1;
    }
}

/// Place the cells of one logical line, wrapping like the native view, and
/// return the number of rows the line occupies and the column behind its
/// last cell.
#[allow(clippy::too_many_arguments)]
fn place_line(
    tokens: impl Iterator<Item = Token>,
    base_row: usize,
    line: usize,
    columns: usize,
    eof_columns: usize,
    tab_width: usize,
    breakat: &str,
    data: &[u8],
    visit: &mut impl FnMut(WrappedCell) -> bool,
    row_end: &mut impl FnMut(usize, usize),
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
    // The cell of the character before the one that is placed. The original
    // editor merges a character that is not wide into the character before it,
    // and the cell is placed only when the next character arrives.
    let mut previous_cell: Option<LineCell> = None;
    // Whether the cell that is held back is on this row.
    let mut previous_on_row = false;
    let mut read_end = 0;
    let mut tokens = tokens.peekable();
    while let Some(token) = tokens.peek() {
        read_end = match token {
            Token::Text { pos } => pos + utf8_length(data[*pos]),
            Token::Tab { pos } | Token::Newline { pos } => pos + 1,
            Token::Eof { pos } => *pos,
        };
        if pending.is_empty() {
            match token {
                Token::Text { pos } => {
                    let (width, len) = token_metrics(data, *pos, out_column);
                    if width == 0 {
                        // The character joins the character before it, even
                        // when that character is on the row before.
                        if let Some(previous) = previous_cell.as_mut() {
                            previous.merged_end = *pos + len;
                            previous.len += len;
                            if previous_on_row {
                                // The cell is on this row, so the row holds
                                // the merged character.
                                if let Some(last) = row_cells.last_mut() {
                                    last.merged_end = previous.merged_end;
                                    last.len = previous.len;
                                }
                            }
                            tokens.next();
                            continue;
                        }
                    }
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
                    pending.push(LineCell {
                        pos: *pos,
                        len,
                        kind,
                        width,
                        column: 0,
                        brk: !breakat.is_empty() && breakat.contains(text),
                        merged_end: 0,
                    });
                }
                Token::Tab { pos } => {
                    let count = tab_width - out_column % tab_width;
                    pending.extend((0..count).map(|index| LineCell {
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
                        merged_end: 0,
                    }));
                }
                Token::Newline { pos } => pending.push(LineCell {
                    pos: *pos,
                    len: 1,
                    kind: CellKind::Newline,
                    width: 1,
                    column: 0,
                    brk: false,
                    merged_end: 0,
                }),
                Token::Eof { pos } => pending.push(LineCell {
                    pos: *pos,
                    len: 0,
                    kind: CellKind::Eof,
                    width: 1,
                    column: 0,
                    brk: false,
                    merged_end: 0,
                }),
            }
        }
        let mut cell = pending.remove(0);
        if previous_break && !cell.brk {
            // The native view records the column after a break character.
            wrap_column = out_column;
        }
        previous_break = cell.brk;
        let width_limit = if cell.kind == CellKind::Eof {
            eof_columns
        } else {
            columns
        };
        if out_column + cell.width > width_limit && !row_cells.is_empty() {
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
                    previous_cell = row_cells.last().cloned();
                    previous_on_row = false;
                    if !emit_row(
                        &row_cells,
                        base_row + out_row,
                        line,
                        cell.pos,
                        visit,
                        row_end,
                    ) {
                        return (out_row, out_column, true);
                    }
                    out_row += 1;
                    out_column = 0;
                    wrap_column = 0;
                    row_cells.clear();
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
                    previous_cell = row_cells.last().cloned();
                    previous_on_row = false;
                    if !emit_row(
                        &row_cells,
                        base_row + out_row,
                        line,
                        cell.pos,
                        visit,
                        row_end,
                    ) {
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
        previous_cell = Some(cell.clone());
        previous_on_row = true;
        row_cells.push(cell);
        if pending.is_empty() {
            tokens.next();
        }
    }
    if !emit_row(
        &row_cells,
        base_row + out_row,
        line,
        read_end.min(data.len()),
        visit,
        row_end,
    ) {
        return (out_row, out_column, true);
    }
    (out_row + 1, out_column, false)
}

/// Emit the cells of one row.
fn emit_row(
    row_cells: &[LineCell],
    row: usize,
    line: usize,
    read_end: usize,
    visit: &mut impl FnMut(WrappedCell) -> bool,
    row_end: &mut impl FnMut(usize, usize),
) -> bool {
    row_end(row, read_end);
    for cell in row_cells {
        if !visit(WrappedCell {
            row,
            line,
            column: cell.column,
            width: cell.width,
            pos: cell.pos,
            len: cell.len,
            kind: cell.kind,
            merged_end: cell.merged_end,
        }) {
            return false;
        }
    }
    true
}

/// The source line displayed in a row.
/// The line number of a byte, counted from the start of the text.
///
/// The original editor reports the line of a position, not the line of a
/// display row, so a view that begins in the middle of a long line still
/// reports the line of the cursor.
pub fn line_number_at_byte(data: &[u8], position: usize) -> usize {
    let position = position.min(data.len());
    1 + data[..position]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

/// The byte the view starts at when a cursor leaves the view.
///
/// The original editor moves the start of the view to the cursor and scrolls
/// it back by half a screen, so the view can begin in the middle of a long
/// line.
pub fn view_start_for_cursor(
    data: &[u8],
    position: usize,
    rows: usize,
    columns: usize,
    _tab_width: usize,
) -> usize {
    let position = position.min(data.len());
    if position == 0 {
        return 0;
    }
    // The original scrolls back by half a screen. It steps back over the
    // bytes, and it stops at the newline that ends the count, so the newline
    // itself is not part of the offset. A newline immediately before the
    // position is skipped first.
    let wanted = (rows / 2) as i64;
    let mut remaining = wanted;
    let mut offset = 0usize;
    let mut index = position - 1;
    if data[index] == b'\n' && index > 0 {
        offset += 1;
        index -= 1;
    }
    // The scan is bounded by the size of a screenful of text, as the original
    // bounds it, so a file without newlines does not scan the whole file.
    let limit = rows * columns;
    loop {
        if data[index] == b'\n' {
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
        offset += 1;
        if offset > limit || index == 0 {
            break;
        }
        index -= 1;
    }
    position - offset
}

/// The byte the view starts at when a line motion moved the cursor out of
/// the view.
///
/// The original editor scrolls the view up by its whole height and then down
/// one row at a time until the cursor is on the last row of the view.
pub fn view_start_for_line_motion(
    data: &[u8],
    position: usize,
    rows: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> usize {
    // The view is scrolled up by half a screen, like native `view_cursors_to`,
    // and then by its whole height, like native `view_redraw_bottom`. The
    // second scroll starts from the byte the first one reached.
    let half = view_start_for_cursor(data, position, rows, physical_columns, tab_width);
    let start = viewport_up(data, half, rows, physical_columns * rows);
    // The view is then scrolled down one row at a time until the cursor is
    // on its last row. The original editor advances the start of the view by
    // the bytes of its first row, and it stops when the cursor is before the
    // end of the view.
    let mut start = start;
    while let Some(end) = view_end(
        data,
        start,
        rows,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
    ) {
        if position < end {
            break;
        }
        let Some((_, len)) = row_bytes(
            data,
            start,
            physical_columns,
            wrap_columns,
            tab_width,
            breakat,
        ) else {
            break;
        };
        let next = start + len;
        if next <= start {
            break;
        }
        start = next;
    }
    start
}

/// The byte the view starts at after it is scrolled up.
///
/// The original editor steps back over the bytes and stops at the newline
/// that ends the count, so the newline itself is not part of the offset. A
/// newline immediately before the position is skipped first, and the scan is
/// bounded by a screenful of text.
fn viewport_up(data: &[u8], position: usize, count: usize, limit: usize) -> usize {
    let position = position.min(data.len());
    if position == 0 {
        return 0;
    }
    let mut remaining = count as isize;
    let mut offset = 0usize;
    let mut index = position - 1;
    if data[index] == b'\n' && index > 0 {
        offset += 1;
        index -= 1;
    }
    loop {
        if data[index] == b'\n' {
            remaining -= 1;
            if remaining == 0 {
                break;
            }
        }
        offset += 1;
        if offset > limit || index == 0 {
            break;
        }
        index -= 1;
    }
    position - offset
}

/// The byte after the last row the view shows.
fn view_end(
    data: &[u8],
    start: usize,
    rows: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut end = None;
    walk_cells_from_layout(
        data,
        start,
        0,
        1,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
        |cell| {
            if cell.row >= rows {
                return false;
            }
            end = Some(cell.pos + cell.len);
            true
        },
    );
    end
}

/// The bytes of the row that starts at a byte.
fn row_bytes(
    data: &[u8],
    start: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<(usize, usize)> {
    let mut end = None;
    let mut first = None;
    walk_cells_from_layout(
        data,
        start,
        0,
        1,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
        |cell| {
            if cell.row > 0 {
                return false;
            }
            first.get_or_insert(cell.pos);
            end = Some(cell.pos + cell.len);
            true
        },
    );
    match (first, end) {
        (Some(first), Some(end)) => Some((end, end - first)),
        _ => None,
    }
}

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
    line_number_at_row_with_layout(data, row, columns, columns, tab_width, breakat)
}

pub fn line_number_at_row_with_layout(
    data: &[u8],
    row: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut line = None;
    walk_cells_with_layout(
        data,
        physical_columns.max(1),
        wrap_columns.max(1),
        tab_width,
        breakat,
        |cell| {
            if cell.row == row {
                line = Some(cell.line);
                return false;
            }
            true
        },
    );
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
    row_line_numbers_with_layout(data, rows, columns, columns, tab_width, breakat)
}

pub fn row_line_numbers_with_layout(
    data: &[u8],
    rows: Range<usize>,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Vec<usize> {
    let mut numbers = Vec::new();
    let mut last: Option<usize> = None;
    walk_cells_with_layout(
        data,
        physical_columns.max(1),
        wrap_columns.max(1),
        tab_width,
        breakat,
        |cell| {
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
        },
    );
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
    screen_position_with_layout(data, position, columns, columns, tab_width, breakat)
}

/// Screen coordinates with separate physical view and soft-wrap widths.
pub fn screen_position_with_layout(
    data: &[u8],
    position: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> (usize, usize) {
    if breakat.is_empty() {
        if let Some(coordinate) = unbroken_position_from(
            data,
            position,
            0,
            0,
            physical_columns,
            wrap_columns,
            tab_width,
            |_, _| {},
        ) {
            return coordinate;
        }
    }
    screen_position_by_cells(
        data,
        position,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
    )
}

fn screen_position_by_cells(
    data: &[u8],
    position: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> (usize, usize) {
    let position = position.min(data.len());
    let mut coordinate = (0, 0);
    walk_cells_with_layout(
        data,
        physical_columns.max(1),
        wrap_columns.max(1),
        tab_width,
        breakat,
        |cell| {
            let found = if position == data.len() {
                cell.kind == CellKind::Eof
            } else {
                cell.pos <= position && position < cell.pos + cell.len
            };
            if found {
                // Native view_coord_get advances over the cell containing an
                // interior byte, including a combining character merged into it.
                coordinate = (
                    cell.row,
                    cell.column + if position > cell.pos { cell.width } else { 0 },
                );
            }
            !found
        },
    );
    coordinate
}

/// Locate a byte in the visible rows of a byte-addressed view.
/// Unlike document coordinates, this never lays out the prefix of the file.
#[allow(clippy::too_many_arguments)]
pub fn screen_position_in_view(
    data: &[u8],
    position: usize,
    start: usize,
    rows: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<(usize, usize)> {
    if position < start || position > data.len() {
        return None;
    }
    let mut coordinate = None;
    walk_cells_from_layout(
        data,
        start,
        0,
        1,
        physical_columns,
        wrap_columns,
        tab_width,
        breakat,
        |cell| {
            if cell.row >= rows {
                return false;
            }
            let found = if position == data.len() {
                cell.kind == CellKind::Eof
            } else {
                cell.pos <= position && position < cell.pos + cell.len
            };
            if found {
                coordinate = Some((cell.row, cell.column));
            }
            !found
        },
    );
    coordinate
}

/// Measure document coordinates without materializing cells when words cannot
/// reflow. ASCII runs advance arithmetically; special characters use the same
/// metrics, tab expansion and delayed soft-wrap rules as `place_line`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn unbroken_position_from(
    data: &[u8],
    position: usize,
    start: usize,
    start_row: usize,
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    mut line_start: impl FnMut(usize, usize),
) -> Option<(usize, usize)> {
    let position = position.min(data.len());
    let columns = wrap_columns.max(1);
    let tab_width = tab_width.clamp(1, 8);
    let mut pos = start.min(data.len());
    let mut row = start_row;
    let mut column = 0;
    let mut previous = None;
    while pos < data.len() {
        if pos > position {
            return None;
        }
        let end = position.saturating_add(1).min(data.len());
        let run = data[pos..end]
            .iter()
            .position(|byte| !(32..127).contains(byte))
            .unwrap_or(end - pos);
        if run > 0 {
            if position < pos + run {
                let offset = column + position - pos;
                return Some((row + offset / columns, offset % columns));
            }
            let offset = column + run - 1;
            row += offset / columns;
            column = offset % columns + 1;
            previous = Some((row, column));
            pos += run;
            continue;
        }
        match data[pos] {
            b'\n' => {
                if column == columns {
                    row += 1;
                    column = 0;
                }
                if position == pos {
                    return Some((row, column));
                }
                pos += 1;
                row += 1;
                column = 0;
                previous = None;
                line_start(pos, row);
            }
            b'\t' => {
                let count = tab_width - column % tab_width;
                for _ in 0..count {
                    if column == columns {
                        row += 1;
                        column = 0;
                    }
                    if position == pos {
                        return Some((row, column));
                    }
                    previous = Some((row, column + 1));
                    column += 1;
                }
                pos += 1;
            }
            byte => {
                if byte >= 128
                    && data
                        .get(pos..pos + utf8_length(byte))
                        .and_then(|bytes| std::str::from_utf8(bytes).ok())
                        .is_none()
                {
                    // Malformed sequences can make merged-cell byte counts
                    // cover skipped bytes. Leave those cases to the cell walk.
                    return None;
                }

                let (width, len) = token_metrics(data, pos, column);
                if width == 0 && previous.is_some() {
                    if pos <= position && position < pos + len {
                        return previous;
                    }
                } else {
                    let width = if width > columns { 1 } else { width };
                    if column + width > columns && column > 0 {
                        row += 1;
                        column = 0;
                    }
                    if pos <= position && position < pos + len {
                        return Some((row, column + if position > pos { width } else { 0 }));
                    }
                    previous = Some((row, column + width));
                    column += width;
                }
                // Match tokenization even for malformed or truncated UTF-8.
                pos += utf8_length(byte);
            }
        }
    }
    if position != data.len() {
        return None;
    }
    if column + 1 > physical_columns.max(1) && column > 0 {
        row += 1;
        column = 0;
    }
    Some((row, column))
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
    last_row_with_layout(data, columns, columns, tab_width, "")
}

pub fn last_row_with_breakat(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    breakat: &str,
) -> usize {
    last_row_with_layout(data, columns, columns, tab_width, breakat)
}

pub fn last_row_with_layout(
    data: &[u8],
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> usize {
    if breakat.is_empty() {
        if let Some((row, _)) = unbroken_position_from(
            data,
            data.len(),
            0,
            0,
            physical_columns,
            wrap_columns,
            tab_width,
            |_, _| {},
        ) {
            return row;
        }
    }
    let mut last = 0;
    walk_cells_with_layout(
        data,
        physical_columns.max(1),
        wrap_columns.max(1),
        tab_width,
        breakat,
        |cell| {
            last = cell.row;
            true
        },
    );
    last
}

/// Index of the last row holding source cells. Native window motions clamp
/// here rather than to the virtual EOF cursor row, so `L` in a short file lands
/// on the last real line as C does.
pub fn last_source_row(data: &[u8], columns: usize, tab_width: usize) -> Option<usize> {
    last_source_row_with_layout(data, columns, columns, tab_width, "")
}

pub fn last_source_row_with_layout(
    data: &[u8],
    physical_columns: usize,
    wrap_columns: usize,
    tab_width: usize,
    breakat: &str,
) -> Option<usize> {
    let mut last = None;
    walk_cells_with_layout(
        data,
        physical_columns.max(1),
        wrap_columns.max(1),
        tab_width,
        breakat,
        |cell| {
            if cell.kind != CellKind::Eof {
                last = Some(cell.row);
            }
            true
        },
    );
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
    fn arithmetic_coordinates_match_the_cell_walk_including_invalid_utf8() {
        let tokens: &[&[u8]] = &[
            b"abc defghijk",
            b"\n",
            b"\t",
            b"\x01",
            b"\x7f",
            b"\xff",
            b"\xe0x",
            "界".as_bytes(),
            "\u{301}".as_bytes(),
            "\u{200d}".as_bytes(),
            "\u{85}".as_bytes(),
            "\u{feff}".as_bytes(),
        ];
        let mut seed = 0x56574557u32;
        for case in 0..160 {
            let mut data = Vec::new();
            for _ in 0..case % 13 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                data.extend_from_slice(tokens[seed as usize % tokens.len()]);
            }
            for columns in [1, 2, 3, 5, 8, 12] {
                for tab in [1, 4, 8] {
                    for physical in [columns, columns + 3] {
                        for position in 0..=data.len() {
                            let reference = screen_position_by_cells(
                                &data, position, physical, columns, tab, "",
                            );
                            let measured = unbroken_position_from(
                                &data,
                                position,
                                0,
                                0,
                                physical,
                                columns,
                                tab,
                                |_, _| {},
                            );
                            let Some(measured) = measured else {
                                assert!(std::str::from_utf8(&data).is_err());
                                continue;
                            };
                            assert_eq!(
                                measured, reference,
                                "data={data:?} position={position} columns={columns} physical={physical} tab={tab}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn a_row_consumer_does_not_decode_the_rest_of_a_long_line() {
        let data = vec![b'x'; 1_000_000];
        let mut decoded = 0;
        let tokens = (0..data.len()).map(|pos| {
            decoded += 1;
            assert!(decoded <= 162, "decoded beyond the requested row");
            Token::Text { pos }
        });
        let (_, _, stopped) = place_line(
            tokens,
            0,
            1,
            80,
            80,
            8,
            "",
            &data,
            &mut |cell| cell.row < 1,
            &mut |_, _| {},
        );
        assert!(stopped);
    }

    #[test]
    fn view_coordinates_start_at_the_requested_byte_and_stop_at_the_view_end() {
        let data = "prefix\n0123456789界a\u{301}tail".as_bytes();
        let start = 10;
        assert_eq!(
            screen_position_in_view(data, start, start, 2, 5, 5, 8, ""),
            Some((0, 0))
        );
        assert_eq!(
            screen_position_in_view(data, start + 5, start, 2, 5, 5, 8, ""),
            Some((1, 0))
        );
        assert_eq!(
            screen_position_in_view(data, 0, start, 2, 5, 5, 8, ""),
            None
        );
        assert_eq!(
            screen_position_in_view(data, data.len(), start, 2, 5, 5, 8, ""),
            None
        );
    }

    #[test]
    fn eof_cell_reflows_the_last_word_at_a_custom_break_character() {
        let data = b"aa bb-cc/dd ee";
        assert_eq!(screen_position_with_breakat(data, 12, 5, 4, " -/"), (4, 0));
        assert_eq!(screen_position_with_breakat(data, 13, 5, 4, " -/"), (4, 1));
        assert_eq!(screen_position_with_breakat(data, 14, 5, 4, " -/"), (4, 2));
    }

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

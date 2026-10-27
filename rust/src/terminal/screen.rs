//! The cells a terminal frame leaves on the screen.
//!
//! The port writes the rows of a frame in the order they are composed, and
//! the original editor writes the screen once and the panels over it, so the
//! byte order differs. The screen they leave behind must be the same, and
//! that is what the tests compare.

/// The sequence that places the cursor.
fn position(sequence: &[u8]) -> Option<(usize, usize)> {
    // The sequence starts with ESC [ and ends with H.
    if sequence.len() < 4 || sequence[0] != 0x1b || sequence[1] != b'[' {
        return None;
    }
    let end = sequence.len() - 1;
    if sequence[end] != b'H' {
        return None;
    }
    let body = &sequence[2..end];
    let text = String::from_utf8_lossy(body);
    let mut parts = text.split(';');
    let row = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
    let column = parts.next().and_then(|part| part.parse().ok()).unwrap_or(1);
    Some((row.max(1), column.max(1)))
}

/// The cells a frame leaves on a screen of the given size.
pub fn render(frame: &str, rows: usize, columns: usize) -> Vec<String> {
    let mut screen = vec![vec![b' '; columns]; rows];
    let bytes = frame.as_bytes();
    let mut row = 1usize;
    let mut column = 1usize;
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == 0x1b {
            // The sequence runs to its final byte.
            let mut end = index + 2;
            while end < bytes.len() && !bytes[end].is_ascii_alphabetic() {
                end += 1;
            }
            let end = end.min(bytes.len() - 1);
            if let Some((new_row, new_column)) = position(&bytes[index..=end]) {
                row = new_row;
                column = new_column;
                index = end + 1;
                continue;
            }
            // A style or an unknown sequence does not change the cells.
            index = (end + 1).min(bytes.len());
            continue;
        }
        if byte == b'\n' {
            row += 1;
            column = 1;
            index += 1;
            continue;
        }
        if byte == b'\r' {
            column = 1;
            index += 1;
            continue;
        }
        if (1..=rows).contains(&row) && (1..=columns).contains(&column) {
            screen[row - 1][column - 1] = byte;
        }
        column += 1;
        index += 1;
    }
    screen
        .into_iter()
        .map(|line| {
            let text = String::from_utf8_lossy(&line);
            text.trim_end().to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_leaves_the_text_it_draws() {
        let frame = "\x1b[1;1Hhello\x1b[2;5Hworld";
        let screen = render(frame, 3, 10);
        assert_eq!(screen[0], "hello");
        assert_eq!(screen[1], "    world");
    }

    #[test]
    fn a_later_write_replaces_an_earlier_one() {
        let frame = "\x1b[1;1Haaaa\x1b[1;1Hbb";
        let screen = render(frame, 2, 10);
        // The later write covers the first two cells; the rest stays.
        assert_eq!(screen[0], "bbaa");
    }

    #[test]
    fn a_style_does_not_change_the_cells() {
        let frame = "\x1b[1;1H\x1b[38;2;1;2;3mhello\x1b[0m";
        let screen = render(frame, 2, 10);
        assert_eq!(screen[0], "hello");
    }
}

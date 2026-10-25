//! Byte-oriented motions and text objects, following `text-motions.c`.
//!
//! Positions include EOF and are clamped to the buffer. Codepoint motions use
//! vis's UTF-8 leading-byte convention, so malformed input remains editable.
//! Character motions attach zero-width combining characters to their preceding
//! base. This is the original wcwidth-style model, not Unicode extended grapheme
//! segmentation. The width table below is a locale-independent approximation;
//! terminal-specific emoji widths and some combining scripts need further work.
use std::ops::Range;

fn continuation(b: u8) -> bool {
    b & 0xc0 == 0x80
}
fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}
fn blank(b: u8) -> bool {
    b == b' ' || b == b'\t'
}
fn at(data: &[u8], pos: usize) -> u8 {
    data.get(pos).copied().unwrap_or(0)
}

pub fn codepoint_next(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    if p < data.len() {
        p += 1;
    }
    while p < data.len() && continuation(data[p]) {
        p += 1;
    }
    p
}
pub fn codepoint_prev(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len()).saturating_sub(1);
    while p > 0 && continuation(data[p]) {
        p -= 1;
    }
    p
}
fn decoded(data: &[u8], pos: usize) -> Option<char> {
    let b = *data.get(pos)?;
    let len = match b {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    std::str::from_utf8(data.get(pos..pos.checked_add(len)?)?)
        .ok()?
        .chars()
        .next()
}
fn zero_width(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036f | 0x0483..=0x0489 | 0x0591..=0x05bd | 0x05bf |
        0x05c1..=0x05c2 | 0x05c4..=0x05c5 | 0x05c7 | 0x0610..=0x061a |
        0x064b..=0x065f | 0x0670 | 0x06d6..=0x06dc | 0x06df..=0x06e4 |
        0x06e7..=0x06e8 | 0x06ea..=0x06ed | 0x0711 | 0x0730..=0x074a |
        0x07a6..=0x07b0 | 0x07eb..=0x07f3 | 0x0816..=0x0819 |
        0x08d3..=0x0902 | 0x093a | 0x093c | 0x0941..=0x0948 | 0x094d |
        0x0951..=0x0957 | 0x0962..=0x0963 | 0x0e31 | 0x0e34..=0x0e3a |
        0x0e47..=0x0e4e | 0x0eb1 | 0x0eb4..=0x0ebc | 0x0ec8..=0x0ecd |
        0x1160..=0x11ff | 0x1ab0..=0x1ace | 0x1dc0..=0x1dff |
        0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 |
        0x2066..=0x206f | 0x20d0..=0x20f0 | 0x2cef..=0x2cf1 |
        0x2de0..=0x2dff | 0x302a..=0x302f | 0x3099..=0x309a |
        0xa66f..=0xa672 | 0xa674..=0xa67d | 0xa69e..=0xa69f |
        0xfe00..=0xfe0f | 0xfe20..=0xfe2f | 0xfeff | 0xfff9..=0xfffb |
        0x1d167..=0x1d169 | 0x1d17b..=0x1d182 | 0x1d185..=0x1d18b |
        0x1d1aa..=0x1d1ad | 0xe0001 | 0xe0020..=0xe007f | 0xe0100..=0xe01ef)
}
pub fn char_next(data: &[u8], pos: usize) -> usize {
    let mut p = codepoint_next(data, pos);
    while p < data.len() && decoded(data, p).is_some_and(zero_width) {
        p = codepoint_next(data, p);
    }
    p
}
pub fn char_prev(data: &[u8], pos: usize) -> usize {
    let mut p = codepoint_prev(data, pos);
    while p > 0 && decoded(data, p).is_some_and(zero_width) {
        p = codepoint_prev(data, p);
    }
    p
}
/// Approximate display width, with tabs occupying one column as in C text motions.
pub fn codepoint_width(data: &[u8], pos: usize) -> usize {
    let Some(c) = decoded(data, pos) else {
        return 1;
    };
    if c == '\t' {
        return 1;
    }
    if c.is_control() {
        return 2;
    }
    if zero_width(c) {
        return 0;
    }
    if matches!(c as u32, 0x1100..=0x115f | 0x2329..=0x232a | 0x2e80..=0x303e |
        0x3040..=0xa4cf | 0xac00..=0xd7a3 | 0xf900..=0xfaff | 0xfe10..=0xfe19 |
        0xfe30..=0xfe6f | 0xff00..=0xff60 | 0xffe0..=0xffe6 | 0x1f300..=0x1faff |
        0x20000..=0x3fffd)
    {
        2
    } else {
        1
    }
}
pub fn line_begin(data: &[u8], pos: usize) -> usize {
    data[..pos.min(data.len())]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |p| p + 1)
}
pub fn line_prev(data: &[u8], pos: usize) -> usize {
    data[..pos.min(data.len())]
        .iter()
        .rposition(|&b| b == b'\n')
        .unwrap_or(0)
}
pub fn line_end(data: &[u8], pos: usize) -> usize {
    let p = pos.min(data.len());
    data[p..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |n| p + n)
}
pub fn line_next(data: &[u8], pos: usize) -> usize {
    let p = line_end(data, pos);
    if p < data.len() { p + 1 } else { p }
}
pub fn line_start(data: &[u8], pos: usize) -> usize {
    let mut p = line_begin(data, pos);
    while p < data.len() && blank(data[p]) {
        p += 1;
    }
    p
}
pub fn line_finish(data: &[u8], pos: usize) -> usize {
    let end = line_end(data, pos);
    if end == 0 || at(data, end - 1) == b'\n' {
        return end;
    }
    let mut p = end - 1;
    while p > 0 && blank(data[p]) {
        p -= 1;
    }
    p + usize::from(data[p] == b'\n')
}
pub fn line_offset(data: &[u8], pos: usize, offset: usize) -> usize {
    let start = line_begin(data, pos);
    start.saturating_add(offset).min(line_end(data, start))
}
pub fn line_char_get(data: &[u8], pos: usize) -> usize {
    let mut p = line_begin(data, pos);
    let mut n = 0;
    while p < pos.min(data.len()) && at(data, p) != b'\n' {
        let q = char_next(data, p);
        if q == p {
            break;
        }
        p = q;
        n += 1;
    }
    n
}
pub fn line_char_set(data: &[u8], pos: usize, count: usize) -> usize {
    let mut p = line_begin(data, pos);
    for _ in 0..count {
        if p == data.len() || data[p] == b'\n' {
            break;
        }
        p = char_next(data, p);
    }
    p
}
pub fn line_width_get(data: &[u8], pos: usize) -> usize {
    let mut p = line_begin(data, pos);
    let mut n = 0;
    while p < pos.min(data.len()) && data[p] != b'\n' {
        n += codepoint_width(data, p);
        p = codepoint_next(data, p);
    }
    n
}
pub fn line_width_set(data: &[u8], pos: usize, width: usize) -> usize {
    let mut p = line_begin(data, pos);
    let mut n = 0;
    while p < data.len() && data[p] != b'\n' {
        n += codepoint_width(data, p);
        if n >= width {
            break;
        }
        p = codepoint_next(data, p);
    }
    p
}
pub fn line_char_next(data: &[u8], pos: usize) -> usize {
    let p = pos.min(data.len());
    if at(data, p) == b'\n' {
        p
    } else {
        char_next(data, p)
    }
}
pub fn line_char_prev(data: &[u8], pos: usize) -> usize {
    let p = char_prev(data, pos);
    if at(data, p) == b'\n' {
        pos.min(data.len())
    } else {
        p
    }
}
pub fn line_up(data: &[u8], pos: usize) -> usize {
    line_width_set(data, line_prev(data, pos), line_width_get(data, pos))
}
pub fn line_down(data: &[u8], pos: usize) -> usize {
    let next = line_next(data, pos);
    if next == data.len() {
        pos.min(data.len())
    } else {
        line_width_set(data, next, line_width_get(data, pos))
    }
}

pub fn find_next(data: &[u8], pos: usize, needle: &[u8]) -> Option<usize> {
    let p = pos.min(data.len());
    if needle.is_empty() {
        return Some(p);
    }
    data[p..]
        .windows(needle.len())
        .position(|s| s == needle)
        .map(|n| p + n)
}
pub fn find_prev(data: &[u8], pos: usize, needle: &[u8]) -> Option<usize> {
    let p = pos.min(data.len());
    if needle.is_empty() {
        return Some(p);
    }
    data[..p].windows(needle.len()).rposition(|s| s == needle)
}
pub fn line_find_next(data: &[u8], pos: usize, needle: &[u8]) -> Option<usize> {
    let p = pos.min(data.len());
    let end = line_next(data, p);
    find_next(&data[..end], p, needle)
}
pub fn line_find_prev(data: &[u8], pos: usize, needle: &[u8]) -> Option<usize> {
    let p = pos.min(data.len());
    let start = line_prev(data, p);
    find_prev(&data[start..p], p - start, needle).map(|n| start + n)
}
fn boundary(b: u8, big: bool) -> bool {
    if big {
        b.is_ascii_whitespace() || b == 0x0b
    } else {
        b.is_ascii() && !(b.is_ascii_alphanumeric() || b == b'_')
    }
}
fn start_next(data: &[u8], pos: usize, big: bool) -> usize {
    let mut p = pos.min(data.len());
    let punct = boundary(at(data, p), big);
    while p < data.len()
        && if punct {
            boundary(at(data, p), big) && !space(at(data, p))
        } else {
            !boundary(at(data, p), big)
        }
    {
        p = char_next(data, p);
    }
    while p < data.len() && space(at(data, p)) {
        p = char_next(data, p);
    }
    p
}
fn start_prev(data: &[u8], pos: usize, big: bool) -> usize {
    let pos = pos.min(data.len());
    if pos == 0 {
        return 0;
    }
    let mut p = char_prev(data, pos);
    while p > 0 && space(at(data, p)) {
        p = char_prev(data, p);
    }
    let punct = boundary(at(data, p), big);
    loop {
        let result = p;
        if p == 0 {
            return result;
        }
        p = char_prev(data, p);
        if !(if punct {
            boundary(at(data, p), big) && !space(at(data, p))
        } else {
            !boundary(at(data, p), big)
        }) {
            return result;
        }
    }
}
fn end_next(data: &[u8], pos: usize, big: bool) -> usize {
    let pos = pos.min(data.len());
    if pos == data.len() {
        return pos;
    }
    let mut p = char_next(data, pos);
    while p < data.len() && space(at(data, p)) {
        p = char_next(data, p);
    }
    let punct = boundary(at(data, p), big);
    loop {
        let result = p;
        if p == data.len() {
            return result;
        }
        p = char_next(data, p);
        if !(if punct {
            boundary(at(data, p), big) && !space(at(data, p))
        } else {
            !boundary(at(data, p), big)
        }) {
            return result;
        }
    }
}
fn end_prev(data: &[u8], pos: usize, big: bool) -> usize {
    let mut p = pos.min(data.len());
    let punct = boundary(at(data, p), big);
    while p > 0
        && if punct {
            boundary(at(data, p), big) && !space(at(data, p))
        } else {
            !boundary(at(data, p), big)
        }
    {
        p = char_prev(data, p);
    }
    while p > 0 && space(at(data, p)) {
        p = char_prev(data, p);
    }
    p
}
pub fn word_start_next(data: &[u8], pos: usize) -> usize {
    start_next(data, pos, false)
}
pub fn word_start_prev(data: &[u8], pos: usize) -> usize {
    start_prev(data, pos, false)
}
pub fn word_end_next(data: &[u8], pos: usize) -> usize {
    end_next(data, pos, false)
}
pub fn word_end_prev(data: &[u8], pos: usize) -> usize {
    end_prev(data, pos, false)
}
pub fn longword_start_next(data: &[u8], pos: usize) -> usize {
    start_next(data, pos, true)
}
pub fn longword_start_prev(data: &[u8], pos: usize) -> usize {
    start_prev(data, pos, true)
}
pub fn longword_end_next(data: &[u8], pos: usize) -> usize {
    end_next(data, pos, true)
}
pub fn longword_end_prev(data: &[u8], pos: usize) -> usize {
    end_prev(data, pos, true)
}

pub fn sentence_next(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    let mut rev = p;
    while rev > 0 && space(at(data, rev)) {
        rev -= 1;
    }
    let mut prev = if rev == 0 { b'.' } else { at(data, rev) };
    while p < data.len() {
        let c = data[p];
        if matches!(prev, b'.' | b'?' | b'!') && space(c) {
            while p < data.len() && space(data[p]) {
                p += 1;
            }
            return p;
        }
        prev = c;
        p += 1;
    }
    p
}
pub fn sentence_prev(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    let mut prev = 0;
    let mut content = false;
    while p > 0 {
        p -= 1;
        let c = data[p];
        if content && space(prev) && matches!(c, b'.' | b'?' | b'!') {
            p += 1;
            while p < data.len() && space(data[p]) {
                p += 1;
            }
            return p;
        }
        content |= !space(c);
        prev = c;
    }
    if content {
        while p < data.len() && space(data[p]) {
            p += 1;
        }
    }
    p
}
pub fn line_blank_next(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    while p < data.len() {
        let n = line_end(data, p);
        if n == data.len() {
            return n;
        }
        p = n + 1;
        while p < data.len() && blank(data[p]) {
            p += 1;
        }
        if at(data, p) == b'\n' {
            return n + 1;
        }
    }
    p
}
pub fn line_blank_prev(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    while p > 0 {
        let Some(n) = data[..p].iter().rposition(|&b| b == b'\n') else {
            return 0;
        };
        p = n;
        if p == 0 {
            return 0;
        }
        p -= 1;
        while p > 0 && blank(data[p]) {
            p -= 1;
        }
        if data[p] == b'\n' {
            return p + 1;
        }
    }
    0
}
pub fn paragraph_next(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    while p < data.len() && (data[p] == b'\n' || blank(data[p])) {
        p = char_next(data, p);
    }
    line_blank_next(data, p)
}
pub fn paragraph_prev(data: &[u8], pos: usize) -> usize {
    let mut p = pos.min(data.len());
    while p > 0 && (at(data, p) == b'\n' || blank(at(data, p))) {
        p = char_prev(data, p);
    }
    line_blank_prev(data, p)
}

fn pair(symbol: u8) -> Option<(u8, u8)> {
    match symbol {
        b'(' | b')' => Some((b'(', b')')),
        b'[' | b']' => Some((b'[', b']')),
        b'{' | b'}' => Some((b'{', b'}')),
        b'<' | b'>' => Some((b'<', b'>')),
        b'"' | b'\'' | b'`' => Some((symbol, symbol)),
        _ => None,
    }
}
fn match_direction(
    data: &[u8],
    pos: usize,
    target: u8,
    forward: bool,
    limits: Range<usize>,
) -> Option<usize> {
    let current = *data.get(pos)?;
    let mut count = 1;
    let mut in_string = false;
    let mut p = pos;
    loop {
        if forward {
            p = p.checked_add(1)?;
        } else {
            p = p.checked_sub(1)?;
        }
        if p >= limits.end || p < limits.start {
            return None;
        }
        let c = *data.get(p)?;
        if c != current && c == b'"' {
            in_string = !in_string;
        }
        if !in_string {
            if c == target {
                count -= 1;
                if count == 0 {
                    return Some(p);
                }
            } else if c == current {
                count += 1;
            }
        }
    }
}
pub fn bracket_match(data: &[u8], pos: usize) -> Option<usize> {
    bracket_match_range(data, pos, 0..data.len())
}
pub fn bracket_match_range(data: &[u8], pos: usize, limits: Range<usize>) -> Option<usize> {
    let current = *data.get(pos)?;
    let (open, close) = pair(current)?;
    if open != close {
        return match_direction(
            data,
            pos,
            if current == open { close } else { open },
            current == open,
            limits,
        );
    }
    let fw = match_direction(data, pos, current, true, limits.clone());
    let bw = match_direction(data, pos, current, false, limits);
    match (fw, bw) {
        (None, b) => b,
        (f, None) => f,
        (Some(f), Some(b)) => {
            let line = line_begin(data, pos);
            if line_begin(data, f) != line {
                Some(b)
            } else if line_begin(data, b) != line {
                Some(f)
            } else if b" \t\n)}]>.,:;\0".contains(&at(data, pos + 1)) {
                Some(b)
            } else {
                Some(f)
            }
        }
    }
}

/// Word range using vis's original byte classification and EOF sentinel.
/// As with the C text object API, callers should check `start <= end`.
pub fn word_object(data: &[u8], pos: usize, outer: bool, big: bool) -> Range<usize> {
    let p = pos.min(data.len());
    let c = at(data, p);
    let prev = if p > 0 { at(data, p - 1) } else { b'0' };
    let next = if p < data.len() {
        at(data, p + 1)
    } else {
        b'0'
    };
    let is_boundary = |c| boundary(c, big);
    let start;
    let end;
    if !outer {
        if space(c) {
            start = char_next(data, end_prev(data, p, big));
            end = start_next(data, p, big);
        } else if is_boundary(c) {
            start = if is_boundary(prev) && !space(prev) {
                start_prev(data, p, big)
            } else {
                p
            };
            end = if is_boundary(next) && !space(next) {
                char_next(data, end_next(data, p, big))
            } else {
                char_next(data, p)
            };
        } else {
            start = if is_boundary(prev) {
                p
            } else {
                start_prev(data, p, big)
            };
            end = if is_boundary(next) {
                char_next(data, p)
            } else {
                char_next(data, end_next(data, p, big))
            };
        }
    } else if space(c) {
        start = char_next(data, end_prev(data, p, big));
        end = char_next(data, end_next(data, p, big));
    } else if is_boundary(prev) && is_boundary(next) {
        if is_boundary(c) {
            start = char_next(data, end_prev(data, p, big));
            end = word_start_next(data, end_next(data, p, big));
        } else {
            start = p;
            end = start_next(data, p, big);
        }
    } else if is_boundary(prev) {
        start = p;
        end = start_next(data, end_next(data, p, big), big);
    } else if is_boundary(next) {
        start = start_prev(data, p, big);
        end = start_next(data, p, big);
    } else {
        start = start_prev(data, p, big);
        end = start_next(data, end_next(data, p, big), big);
    }
    start..end
}
pub fn line_object(data: &[u8], pos: usize, inner: bool) -> Range<usize> {
    let r = line_begin(data, pos)..line_next(data, pos);
    if inner { range_inner(data, r) } else { r }
}
pub fn sentence_object(data: &[u8], pos: usize) -> Range<usize> {
    sentence_prev(data, pos)..sentence_next(data, pos)
}
pub fn delimited_object(data: &[u8], pos: usize, symbol: u8, outer: bool) -> Option<Range<usize>> {
    let (open, close) = pair(symbol)?;
    if pos >= data.len() {
        return None;
    }
    if open == close && matches!(data[pos], b'"' | b'\'' | b'`') {
        let m = bracket_match(data, pos)?;
        let a = pos.min(m);
        let b = pos.max(m);
        return Some(if outer { a..b + 1 } else { a + 1..b });
    }
    let mut count = 1;
    let mut start = None;
    for p in (0..=pos).rev() {
        if data[p] == open {
            count -= 1;
            if count == 0 {
                start = Some(p);
                break;
            }
        } else if data[p] == close && p != pos {
            count += 1;
        }
    }
    let start = start?;
    count = 1;
    for (p, &c) in data.iter().enumerate().skip(pos) {
        if c == close {
            count -= 1;
            if count == 0 {
                return Some(if outer { start..p + 1 } else { start + 1..p });
            }
        } else if c == open && p != pos {
            count += 1;
        }
    }
    None
}
pub fn paragraph_object(data: &[u8], pos: usize, outer: bool) -> Range<usize> {
    let p = pos.min(data.len());
    let bol = line_begin(data, p);
    let is_blank = data[bol..line_end(data, p)].iter().all(|&b| blank(b));
    let range = if is_blank {
        let mut prev = p;
        while prev > 0 && (at(data, prev) == b'\n' || blank(at(data, prev))) {
            prev -= 1;
        }
        let start = if at(data, prev) == b'\n' || blank(at(data, prev)) {
            prev
        } else {
            line_next(data, prev)
        };
        let mut next = p;
        while next < data.len() && (data[next] == b'\n' || blank(data[next])) {
            next += 1;
        }
        let end = if next == data.len() {
            prev
        } else {
            line_begin(data, next)
        };
        start..end
    } else {
        let mut start = line_blank_prev(data, p);
        if start > 0 || at(data, start) == b'\n' {
            start = line_next(data, start);
        }
        start..line_blank_next(data, p)
    };
    if outer {
        let next = paragraph_object(data, range.end, false);
        range.start.min(next.start)..range.end.max(next.end)
    } else {
        range
    }
}
pub fn block_start(data: &[u8], pos: usize) -> usize {
    pos.checked_sub(1)
        .and_then(|p| delimited_object(data, p, b'{', false))
        .map_or(pos.min(data.len()), |r| r.start.saturating_sub(1))
}
pub fn block_end(data: &[u8], pos: usize) -> usize {
    pos.checked_add(1)
        .and_then(|p| delimited_object(data, p, b'{', false))
        .map_or(pos.min(data.len()), |r| r.end)
}
pub fn parenthesis_start(data: &[u8], pos: usize) -> usize {
    pos.checked_sub(1)
        .and_then(|p| delimited_object(data, p, b'(', false))
        .map_or(pos.min(data.len()), |r| r.start.saturating_sub(1))
}
pub fn parenthesis_end(data: &[u8], pos: usize) -> usize {
    pos.checked_add(1)
        .and_then(|p| delimited_object(data, p, b'(', false))
        .map_or(pos.min(data.len()), |r| r.end)
}
pub fn word_find_next(data: &[u8], pos: usize, word: &[u8]) -> Option<Range<usize>> {
    if word.is_empty() {
        return None;
    }
    let mut p = pos.min(data.len());
    loop {
        let m = find_next(data, p, word)?;
        if m == p {
            return None;
        }
        let r = word_object(data, m, false, false);
        if r.end.checked_sub(r.start) == Some(word.len()) {
            return Some(r);
        }
        if r.end <= p {
            return None;
        }
        p = r.end;
    }
}
pub fn word_find_prev(data: &[u8], pos: usize, word: &[u8]) -> Option<Range<usize>> {
    if word.is_empty() {
        return None;
    }
    let mut p = pos.min(data.len());
    loop {
        let m = find_prev(data, p, word)?;
        if m == p {
            return None;
        }
        let r = word_object(data, m, false, false);
        if r.end.checked_sub(r.start) == Some(word.len()) {
            return Some(r);
        }
        p = m;
    }
}
pub fn range_inner(data: &[u8], range: Range<usize>) -> Range<usize> {
    let mut start = range.start.min(data.len());
    let mut end = range.end.min(data.len()).max(start);
    while start < end && space(data[start]) {
        start += 1;
    }
    while end > start && space(data[end - 1]) {
        end -= 1;
    }
    start..end
}
pub fn range_linewise(data: &[u8], range: Range<usize>) -> Range<usize> {
    let end = range.end.min(data.len());
    line_begin(data, range.start)..if end == line_begin(data, end) {
        end
    } else {
        line_next(data, end)
    }
}
pub fn range_is_linewise(data: &[u8], range: Range<usize>) -> bool {
    range.start < range.end
        && range.end <= data.len()
        && range.start == line_begin(data, range.start)
        && range.end == line_begin(data, range.end)
}
pub fn indentation_object(data: &[u8], pos: usize) -> Range<usize> {
    let bol = line_begin(data, pos);
    let sol = line_start(data, bol);
    let indent = &data[bol..sol];
    let empty = at(data, bol) == b'\n';
    let mut start = bol;
    let mut end = line_next(data, bol);
    let accepts = |p| {
        let e = at(data, p) == b'\n';
        let s = line_start(data, p);
        s - p >= indent.len()
            && (!empty || e)
            && (!indent.is_empty() || !e)
            && data.get(p..p + indent.len()) == Some(indent)
    };
    while start > 0 {
        let p = line_begin(data, line_prev(data, start));
        if p == start || !accepts(p) {
            break;
        }
        start = p;
    }
    while end < data.len() && accepts(end) {
        let p = line_next(data, end);
        if p == end {
            break;
        }
        end = p;
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn line_boundaries_and_width() {
        let d = b"  abc  \n\tde\nlast";
        assert_eq!(line_begin(d, 4), 0);
        assert_eq!(line_start(d, 4), 2);
        assert_eq!(line_finish(d, 4), 4);
        assert_eq!(line_end(d, 4), 7);
        assert_eq!(line_next(d, 4), 8);
        assert_eq!(line_prev(d, 9), 7);
        assert_eq!(line_begin(d, 8), 8);
        assert_eq!(line_up(d, 9), 0);
        assert_eq!(line_down(d, 2), 9);
        assert_eq!(line_down(d, 14), 14);
        assert_eq!(line_object(d, 3, true), 2..5);
    }
    #[test]
    fn utf8_combining_binary_and_eof() {
        let d = "a\u{301}中b".as_bytes();
        assert_eq!(codepoint_next(d, 0), 1);
        assert_eq!(char_next(d, 0), 3);
        assert_eq!(char_prev(d, 3), 0);
        assert_eq!(char_prev(d, 6), 3);
        assert_eq!(line_width_get(d, 6), 3);
        assert_eq!(char_next(d, d.len()), d.len());
        assert_eq!(char_prev(d, 0), 0);
        assert_eq!(codepoint_next(&[0xff, 0x80, 0, 0x81, b'x'], 0), 2);
        assert_eq!(char_next(&[0xff, 0, b'x'], 0), 1);
        assert_eq!(char_next(&[], usize::MAX), 0);
    }
    #[test]
    fn word_motions_punctuation_and_single_letters() {
        let d = b"a b c d : single letter words\nfoo_bar+-baz\n";
        assert_eq!(word_end_next(d, 0), 2);
        assert_eq!(word_start_next(d, 0), 2);
        assert_eq!(word_start_prev(d, 2), 0);
        assert_eq!(word_end_prev(d, 10), 8);
        let p = find_next(d, 0, b"foo").unwrap();
        assert_eq!(word_end_next(d, p), p + 6);
        assert_eq!(word_start_next(d, p), p + 7);
        assert_eq!(word_start_next(d, p + 7), p + 9);
        assert_eq!(longword_end_next(d, p), p + 11);
        assert_eq!(word_object(d, p + 3, false, false), p..p + 7);
        assert_eq!(word_object(b"one  two", 4, true, false), 3..8);
    }
    #[test]
    fn bracket_nesting_and_strings() {
        let d = b"{ a { b } \"}\" c }";
        assert_eq!(bracket_match(d, 0), Some(d.len() - 1));
        assert_eq!(bracket_match(d, d.len() - 1), Some(0));
        assert_eq!(delimited_object(d, 6, b'{', false), Some(5..8));
        assert_eq!(delimited_object(d, 6, b'{', true), Some(4..9));
        assert_eq!(bracket_match(b"(no", 0), None);
        assert_eq!(bracket_match_range(b"(a)", 0, 0..2), None);
    }
    #[test]
    fn paragraphs_sentences_indentation() {
        let d = b"One. Two!\n\nThree\n  \nLast";
        assert_eq!(sentence_next(d, 0), 5);
        assert_eq!(sentence_prev(d, 8), 5);
        assert_eq!(paragraph_next(d, 0), 10);
        assert_eq!(paragraph_next(d, 10), 17);
        assert_eq!(paragraph_prev(d, 22), 17);
        let d = b"top\n  one\n    two\n  three\nlast\n";
        assert_eq!(indentation_object(d, 6), 4..26);
    }
    #[test]
    fn c_whitespace_classification_and_eof_sentinel() {
        // C IsSpace excludes VT/FF, while longword's isspace includes them.
        let d = b"ab\x0bbc\x0ccd";
        assert_eq!(word_start_next(d, 0), 2);
        assert_eq!(longword_start_next(d, 0), 2);
        assert_eq!(longword_start_next(d, 2), 3);
        assert_eq!(word_end_next(b"+-", 0), 2);
        assert_eq!(word_end_next(b"ab", 0), 1);
        assert_eq!(longword_end_next(b"ab", 0), 2);
        // The original C EOF sentinel participates in word objects.
        assert_eq!(word_object(b"ab", 2, false, true), 0..2);
        assert_eq!(word_object(b"a b c", 1, true, false), 1..3);
    }
    #[test]
    fn paragraph_objects_and_literal_words() {
        let d = b"first\nline\n\nsecond\n";
        assert_eq!(paragraph_object(d, 2, false), 0..11);
        assert_eq!(paragraph_object(d, 11, false), 11..12);
        assert_eq!(paragraph_object(d, 2, true), 0..12);
        assert_eq!(word_find_next(b"cat cats cat", 0, b"cat"), None);
        assert_eq!(word_find_next(b"cat cats cat", 1, b"cat"), Some(9..12));
        assert_eq!(word_find_prev(b"cat cats cat", 9, b"cat"), Some(0..3));
    }
    #[test]
    fn byte_positions_never_escape_buffer() {
        for d in [
            b"".as_slice(),
            b"\n\n",
            b" a+-\xff\x80\0b\n",
            "x\u{301}\n中".as_bytes(),
        ] {
            for p in 0..=d.len() + 1 {
                for f in [
                    char_next,
                    char_prev,
                    codepoint_next,
                    codepoint_prev,
                    line_begin,
                    line_start,
                    line_finish,
                    line_end,
                    line_next,
                    line_prev,
                    line_up,
                    line_down,
                    word_start_next,
                    word_start_prev,
                    word_end_next,
                    word_end_prev,
                    longword_start_next,
                    longword_start_prev,
                    longword_end_next,
                    longword_end_prev,
                    paragraph_next,
                    paragraph_prev,
                    sentence_next,
                    sentence_prev,
                ] {
                    assert!(f(d, p) <= d.len());
                }
            }
        }
    }
}

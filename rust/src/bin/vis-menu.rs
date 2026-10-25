/*
 * Menu semantics ported from vis-menu.c.
 * MIT/X Consortium License
 * © 2011 Rafael Garcia Gallego <rafael.garcia.gallego@gmail.com>
 * Based on dmenu:
 * © 2010-2011 Connor Lane Smith <cls@lubutu.com>
 * © 2006-2011 Anselm R Garbe <anselm@garbe.us>
 * © 2009 Gottox <gottox@s01.de>
 * © 2009 Markus Schnalke <meillo@marmaro.de>
 * © 2009 Evan Gates <evan.gates@gmail.com>
 * © 2006-2008 Sander van Dijk <a dot h dot vandijk at gmail dot com>
 * © 2006-2007 Michał Janeczek <janeczek at gmail dot com>
 *
 * Permission is hereby granted, free of charge, to any person obtaining a
 * copy of this software and associated documentation files (the "Software"),
 * to deal in the Software without restriction, including without limitation
 * the rights to use, copy, modify, merge, publish, distribute, sublicense,
 * and/or sell copies of the Software, and to permit persons to whom the
 * Software is furnished to do so, subject to the following conditions:
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL
 * THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
 * DEALINGS IN THE SOFTWARE.
 */
//! Inline terminal menu. Standard input supplies candidates; the controlling
//! terminal supplies keys, and only the selected byte string reaches stdout.
//! Query text is bounded to 64 KiB, independent of the platform's C BUFSIZ;
//! logical candidate lines are not split at that implementation buffer size.
#![forbid(unsafe_code)]
#![cfg_attr(not(all(unix, feature = "terminal")), allow(dead_code))]

use std::ffi::OsString;
use std::io::{self, BufRead, Write};

const MAX_QUERY: usize = 65_535;
const USAGE: &str = "usage: vis-menu-rs [-b|-t] [-i] [-l lines] [-p prompt] [initial selection]";

#[derive(Default)]
struct Options {
    insensitive: bool,
    position: i8,
    lines: u64,
    prompt: Vec<u8>,
    initial: Vec<u8>,
}

enum Parsed {
    Run(Options),
    Version,
}

fn line_count(bytes: &[u8]) -> Result<u64, &'static str> {
    // Keep the original integer_conversion contract: decimal/hex, optional
    // minus, and an ignored suffix. Clamp to the terminal height at rendering.
    let negative = bytes.first() == Some(&b'-');
    let mut digits = &bytes[usize::from(negative)..];
    let radix = if !negative && digits.len() > 2 && matches!(&digits[..2], b"0x" | b"0X") {
        digits = &digits[2..];
        16
    } else if digits.len() > 1 && digits[0] == b'#' {
        digits = &digits[1..];
        16
    } else {
        10
    };
    let mut value = 0u64;
    for &digit in digits {
        let number = match digit {
            b'0'..=b'9' => u64::from(digit - b'0'),
            b'a'..=b'f' => u64::from(digit - b'a') + 10,
            b'A'..=b'F' => u64::from(digit - b'A') + 10,
            _ => break,
        };
        if number >= radix {
            break;
        }
        value = value
            .checked_mul(radix)
            .and_then(|value| value.checked_add(number))
            .ok_or(USAGE)?;
    }
    Ok(if negative {
        value.wrapping_neg()
    } else {
        value
    })
}

fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Parsed, &'static str> {
    let mut options = Options::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_encoded_bytes() {
            b"-v" => return Ok(Parsed::Version),
            b"-i" => options.insensitive = true,
            b"-t" => options.position = 1,
            b"-b" => options.position = -1,
            b"-p" => options.prompt = arguments.next().ok_or(USAGE)?.as_encoded_bytes().to_vec(),
            b"-l" => options.lines = line_count(arguments.next().ok_or(USAGE)?.as_encoded_bytes())?,
            bytes if !bytes.starts_with(b"-") => {
                options.initial = bytes[..bytes.len().min(MAX_QUERY)].to_vec()
            }
            _ => return Err(USAGE),
        }
    }
    Ok(Parsed::Run(options))
}

/// Byte-line input with the original menu's NUL-terminated textual item model.
/// Unlike fgets, long logical lines remain one candidate instead of splitting.
pub fn read_candidates(mut input: impl BufRead) -> io::Result<Vec<Vec<u8>>> {
    let mut items = Vec::new();
    loop {
        let mut line = Vec::new();
        if input.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if let Some(nul) = line.iter().position(|&byte| byte == 0) {
            line.truncate(nul);
        }
        items.push(line);
    }
    Ok(items)
}

fn equal(a: &[u8], b: &[u8], insensitive: bool) -> bool {
    if insensitive {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Stable exact/prefix/substring ordering, with space-delimited AND filters.
pub fn rank_matches(items: &[Vec<u8>], query: &[u8], insensitive: bool) -> Vec<usize> {
    let tokens: Vec<_> = query
        .split(|&byte| byte == b' ')
        .filter(|token| !token.is_empty())
        .collect();
    let mut exact = Vec::new();
    let mut prefix = Vec::new();
    let mut substring = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if !tokens.iter().all(|token| {
            item.windows(token.len())
                .any(|window| equal(window, token, insensitive))
        }) {
            continue;
        }
        if tokens.is_empty() || equal(item, query, insensitive) {
            exact.push(index);
        } else if item
            .get(..tokens[0].len())
            .is_some_and(|start| equal(start, tokens[0], insensitive))
        {
            prefix.push(index);
        } else {
            substring.push(index);
        }
    }
    exact.extend(prefix);
    exact.extend(substring);
    exact
}

#[derive(Clone, Copy, Debug)]
enum Key {
    Byte(u8),
    Enter,
    AcceptText,
    Cancel,
    Previous,
    Next,
    Left,
    Right,
    Home,
    End,
    Backspace,
    Delete,
    Tab,
    KillEnd,
    KillStart,
    KillWord,
    WordBack,
    WordForward,
    DeleteWord,
    PageUp,
    PageDown,
    Ignore,
}

struct Menu {
    items: Vec<Vec<u8>>,
    query: Vec<u8>,
    cursor: usize,
    matches: Vec<usize>,
    selected: usize,
    page: usize,
    next: usize,
    previous: usize,
    insensitive: bool,
    vertical: bool,
}

fn rune_next(bytes: &[u8], pos: usize) -> usize {
    let mut next = (pos + 1).min(bytes.len());
    while next < bytes.len() && bytes[next] & 0xc0 == 0x80 {
        next += 1;
    }
    next
}
fn rune_prev(bytes: &[u8], pos: usize) -> usize {
    let mut previous = pos.saturating_sub(1);
    while previous > 0 && bytes[previous] & 0xc0 == 0x80 {
        previous -= 1;
    }
    previous
}

impl Menu {
    fn new(items: Vec<Vec<u8>>, options: &Options) -> Self {
        let mut menu = Self {
            items,
            query: options.initial.clone(),
            cursor: options.initial.len(),
            matches: Vec::new(),
            selected: 0,
            page: 0,
            next: 0,
            previous: 0,
            insensitive: options.insensitive,
            vertical: options.lines > 0,
        };
        menu.rematch();
        menu
    }
    fn rematch(&mut self) {
        self.matches = rank_matches(&self.items, &self.query, self.insensitive);
        self.selected = 0;
        self.page = 0;
    }
    fn remove(&mut self, range: std::ops::Range<usize>) {
        self.query.drain(range.clone());
        self.cursor = range.start;
        self.rematch();
    }
    fn insert(&mut self, bytes: &[u8]) {
        if self.query.len().saturating_add(bytes.len()) > MAX_QUERY {
            return;
        }
        self.query
            .splice(self.cursor..self.cursor, bytes.iter().copied());
        self.cursor += bytes.len();
        self.rematch();
    }
    fn item_width(&self, index: usize, capacity: usize) -> usize {
        (self.items[self.matches[index]].len() + 4).min(capacity)
    }
    fn paginate(&mut self, capacity: usize) {
        let capacity = capacity.max(1);
        self.page = self.page.min(self.matches.len().saturating_sub(1));
        let mut used = 0;
        self.next = self.page;
        while self.next < self.matches.len() {
            let width = if self.vertical {
                1
            } else {
                self.item_width(self.next, capacity)
            };
            if used + width > capacity {
                break;
            }
            used += width;
            self.next += 1;
        }
        used = 0;
        self.previous = self.page;
        while self.previous > 0 {
            let width = if self.vertical {
                1
            } else {
                self.item_width(self.previous - 1, capacity)
            };
            if used + width > capacity {
                break;
            }
            used += width;
            self.previous -= 1;
        }
    }
    fn keep_visible(&mut self, capacity: usize) {
        self.paginate(capacity);
        while self.selected >= self.next && self.next < self.matches.len() {
            self.page = self.next;
            self.paginate(capacity);
        }
        while self.selected < self.page {
            self.page = self.previous;
            self.paginate(capacity);
        }
    }
    fn key(&mut self, key: Key, capacity: usize) -> Option<Result<Vec<u8>, ()>> {
        match key {
            Key::Cancel => return Some(Err(())),
            Key::Enter => {
                return Some(Ok(self.matches.get(self.selected).map_or_else(
                    || self.query.clone(),
                    |&index| self.items[index].clone(),
                )));
            }
            Key::AcceptText => return Some(Ok(self.query.clone())),
            Key::Byte(byte) if byte >= 32 && byte != 127 => self.insert(&[byte]),
            Key::Home if self.selected == 0 => self.cursor = 0,
            Key::Home => {
                self.selected = 0;
                self.page = 0;
            }
            Key::End if self.cursor < self.query.len() => self.cursor = self.query.len(),
            Key::End => self.selected = self.matches.len().saturating_sub(1),
            Key::Left
                if self.cursor > 0
                    && (self.matches.is_empty() || self.selected == 0 || self.vertical) =>
            {
                self.cursor = rune_prev(&self.query, self.cursor)
            }
            Key::Previous | Key::Left => self.selected = self.selected.saturating_sub(1),
            Key::Right if self.cursor < self.query.len() => {
                self.cursor = rune_next(&self.query, self.cursor)
            }
            Key::Next | Key::Right => {
                self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1))
            }
            Key::Delete if self.cursor < self.query.len() => {
                self.remove(self.cursor..rune_next(&self.query, self.cursor))
            }
            Key::Backspace if self.cursor > 0 => {
                self.remove(rune_prev(&self.query, self.cursor)..self.cursor)
            }
            Key::Tab => {
                if let Some(&index) = self.matches.get(self.selected) {
                    self.query =
                        self.items[index][..self.items[index].len().min(MAX_QUERY)].to_vec();
                    self.cursor = self.query.len();
                    self.rematch();
                }
            }
            Key::KillEnd => self.remove(self.cursor..self.query.len()),
            Key::KillStart => self.remove(0..self.cursor),
            Key::KillWord | Key::WordBack => {
                let end = self.cursor;
                while self.cursor > 0 && self.query[rune_prev(&self.query, self.cursor)] == b' ' {
                    self.cursor = rune_prev(&self.query, self.cursor);
                }
                while self.cursor > 0 && self.query[rune_prev(&self.query, self.cursor)] != b' ' {
                    self.cursor = rune_prev(&self.query, self.cursor);
                }
                if matches!(key, Key::KillWord) {
                    self.remove(self.cursor..end);
                }
            }
            Key::WordForward => {
                while self.cursor < self.query.len()
                    && self.query.get(rune_next(&self.query, self.cursor)) == Some(&b' ')
                {
                    self.cursor = rune_next(&self.query, self.cursor);
                }
                if self.cursor < self.query.len() {
                    loop {
                        self.cursor = rune_next(&self.query, self.cursor);
                        if self.cursor == self.query.len() || self.query[self.cursor] == b' ' {
                            break;
                        }
                    }
                }
            }
            Key::DeleteWord => {
                while self.cursor < self.query.len()
                    && self.query.get(rune_next(&self.query, self.cursor)) == Some(&b' ')
                {
                    self.remove(self.cursor..rune_next(&self.query, self.cursor));
                }
                if self.cursor < self.query.len() {
                    loop {
                        self.remove(self.cursor..rune_next(&self.query, self.cursor));
                        if self.cursor == self.query.len() || self.query[self.cursor] == b' ' {
                            break;
                        }
                    }
                }
            }
            Key::PageUp => {
                self.selected = self.previous;
                self.page = self.previous;
            }
            Key::PageDown if self.next < self.matches.len() => {
                self.selected = self.next;
                self.page = self.next;
            }
            _ => {}
        }
        self.keep_visible(capacity);
        None
    }
}

#[cfg(all(unix, feature = "terminal"))]
mod interactive {
    use super::*;
    use rustix::event::{PollFd, PollFlags, Timespec, poll};
    use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};
    use std::collections::VecDeque;
    use std::fs::{File, OpenOptions};
    use std::io::{IsTerminal, Read};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    struct Tty {
        file: File,
        saved: Termios,
        active: bool,
        lines: usize,
        position: i8,
    }
    impl Tty {
        fn new(options: &Options) -> io::Result<Self> {
            let name = if io::stderr().is_terminal() {
                rustix::termios::ttyname(io::stderr(), Vec::new())?
            } else if io::stdout().is_terminal() {
                rustix::termios::ttyname(io::stdout(), Vec::new())?
            } else if io::stdin().is_terminal() {
                rustix::termios::ttyname(io::stdin(), Vec::new())?
            } else {
                let tty = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
                rustix::termios::ttyname(&tty, Vec::new())?
            };
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as _)
                .open(std::ffi::OsStr::from_bytes(name.to_bytes()))?;
            let saved = tcgetattr(&file)?;
            let mut raw = saved.clone();
            raw.make_raw();
            tcsetattr(&file, OptionalActions::Now, &raw)?;
            let mut tty = Self {
                file,
                saved,
                active: true,
                lines: 0,
                position: options.position,
            };
            let (_, rows) = tty.size()?;
            tty.lines = options.lines.min(rows.saturating_sub(1) as u64) as usize;
            if options.position == 0 && tty.lines > 0 {
                tty.file.write_all("\r\n".repeat(tty.lines).as_bytes())?;
                write!(tty.file, "\x1b[{}F", tty.lines)?;
            }
            tty.file.write_all(b"\x1b7\x1b[?2004h")?;
            tty.file.flush()?;
            Ok(tty)
        }
        fn size(&self) -> io::Result<(usize, usize)> {
            let size = tcgetwinsize(&self.file)?;
            Ok((
                usize::from(size.ws_col).max(1),
                usize::from(size.ws_row).max(1),
            ))
        }
        fn restore(&mut self) -> io::Result<()> {
            if !self.active {
                return Ok(());
            }
            let output = self
                .file
                .write_all(if self.position == 0 {
                    b"\x1b[0m\x1b[?2004l\r\n"
                } else {
                    b"\x1b[0m\x1b[?2004l\r\x1b[K"
                })
                .and_then(|()| self.file.flush());
            let settings =
                tcsetattr(&self.file, OptionalActions::Now, &self.saved).map_err(io::Error::from);
            if settings.is_ok() {
                self.active = false;
            }
            output.and(settings)
        }
        fn draw(&mut self, menu: &mut Menu, options: &Options) -> io::Result<usize> {
            let (columns, rows) = self.size()?;
            let lines = options.lines.min(rows.saturating_sub(1) as u64) as usize;
            let prompt_width = if options.prompt.is_empty() {
                0
            } else {
                (options.prompt.len() + 4).min(columns)
            };
            let input_width = if lines > 0 || menu.matches.is_empty() {
                columns.saturating_sub(prompt_width)
            } else {
                menu.items
                    .iter()
                    .map(|item| item.len() + 4)
                    .max()
                    .unwrap_or(4)
                    .min(columns / 3)
            };
            let capacity = if lines > 0 {
                lines
            } else {
                columns
                    .saturating_sub(prompt_width + input_width + 10)
                    .max(1)
            };
            menu.keep_visible(capacity);
            let mut frame = if options.position > 0 {
                "\x1b[1;1H".to_owned()
            } else if options.position < 0 {
                format!("\x1b[{};1H", rows.saturating_sub(lines).max(1))
            } else {
                "\x1b8\r".to_owned()
            };
            frame.push_str("\x1b[0m\x1b[K");
            if prompt_width > 0 {
                frame.push_str("\x1b[7m");
                frame.push_str(&label(&options.prompt, prompt_width));
                frame.push_str("\x1b[0m");
            }
            frame.push_str(&label(&menu.query, input_width));
            if lines > 0 {
                for row in 0..lines {
                    frame.push_str("\r\n\x1b[K");
                    let index = menu.page + row;
                    if index < menu.next {
                        if index == menu.selected {
                            frame.push_str("\x1b[7m");
                        }
                        frame.push_str(&label(&menu.items[menu.matches[index]], columns));
                        frame.push_str("\x1b[0m");
                    }
                }
                frame.push_str(&format!("\x1b[{lines}F"));
            } else {
                let mut remaining = columns.saturating_sub(prompt_width + input_width);
                if menu.page > 0 {
                    let arrow = label(b"<", remaining.min(5));
                    remaining = remaining.saturating_sub(5);
                    frame.push_str(&arrow);
                }
                for index in menu.page..menu.next {
                    let width = (menu.items[menu.matches[index]].len() + 4).min(remaining);
                    if index == menu.selected {
                        frame.push_str("\x1b[7m");
                    }
                    frame.push_str(&label(&menu.items[menu.matches[index]], width));
                    frame.push_str("\x1b[0m");
                    remaining = remaining.saturating_sub(width);
                }
                if menu.next < menu.matches.len() && columns >= 5 {
                    frame.push_str(&format!("\x1b[{}G", columns - 4));
                    frame.push_str(&label(b">", 5));
                }
            }
            let cursor = (prompt_width + 3 + menu.cursor).min(columns).max(1);
            frame.push_str(&format!("\x1b[{cursor}G"));
            self.file.write_all(frame.as_bytes())?;
            self.file.flush()?;
            self.lines = lines;
            Ok(capacity)
        }
    }
    impl Drop for Tty {
        fn drop(&mut self) {
            let _ = self.restore();
        }
    }

    // Render escaped user text; candidate bytes can never emit terminal commands.
    fn label(bytes: &[u8], width: usize) -> String {
        if width < 5 {
            return " ".repeat(width);
        }
        let mut text = String::new();
        for character in String::from_utf8_lossy(bytes).chars() {
            if character.is_control() {
                text.extend(character.escape_default());
            } else {
                text.push(character);
            }
        }
        let capacity = width - 4;
        let count = text.chars().count();
        let mut text: String = text.chars().take(capacity).collect();
        if count > capacity {
            text = text.chars().take(capacity.saturating_sub(1)).collect();
            text.push('.');
        }
        let padding = capacity.saturating_sub(text.chars().count());
        format!("  {text}{}  ", " ".repeat(padding))
    }

    fn decode(bytes: &mut VecDeque<u8>) -> Option<Key> {
        let first = *bytes.front()?;
        if first == 27 {
            if bytes.len() < 2 {
                return None;
            }
            let second = bytes[1];
            if second == b'[' || second == b'O' {
                let end = (2..bytes.len()).find(|&index| (0x40..=0x7e).contains(&bytes[index]))?;
                let final_byte = bytes[end];
                let parameter = bytes.get(2).copied();
                let key = match final_byte {
                    b'A' => Key::Previous,
                    b'B' => Key::Next,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    b'~' => match parameter {
                        Some(b'1' | b'7') => Key::Home,
                        Some(b'3') => Key::Delete,
                        Some(b'4' | b'8') => Key::End,
                        Some(b'5') => Key::PageUp,
                        Some(b'6') => Key::PageDown,
                        _ => Key::Ignore,
                    },
                    _ => Key::Ignore,
                };
                bytes.drain(..=end);
                return Some(key);
            }
            bytes.drain(..2);
            return Some(match second {
                27 => Key::Cancel,
                b'b' => Key::WordBack,
                b'f' => Key::WordForward,
                b'd' => Key::DeleteWord,
                b'v' => Key::PageDown,
                _ => Key::Ignore,
            });
        }
        bytes.pop_front();
        Some(match first {
            3 => Key::Cancel,
            10 | 13 => Key::Enter,
            28 | 29 => Key::AcceptText,
            1 => Key::Home,
            5 => Key::End,
            2 => Key::Left,
            6 => Key::Right,
            16 => Key::Previous,
            14 => Key::Next,
            4 => Key::Delete,
            8 | 127 => Key::Backspace,
            9 => Key::Tab,
            11 => Key::KillEnd,
            21 => Key::KillStart,
            23 => Key::KillWord,
            22 => Key::PageUp,
            byte if byte >= 32 => Key::Byte(byte),
            _ => Key::Ignore,
        })
    }

    pub fn run(menu: &mut Menu, options: &Options) -> io::Result<Option<Vec<u8>>> {
        let terminated = Arc::new(AtomicBool::new(false));
        let mut registrations = Vec::new();
        for signal in [
            signal_hook::consts::SIGTERM,
            signal_hook::consts::SIGHUP,
            signal_hook::consts::SIGINT,
            signal_hook::consts::SIGQUIT,
        ] {
            registrations.push(signal_hook::flag::register(
                signal,
                Arc::clone(&terminated),
            )?);
        }
        struct Signals(Vec<signal_hook::SigId>);
        impl Drop for Signals {
            fn drop(&mut self) {
                for id in self.0.drain(..) {
                    signal_hook::low_level::unregister(id);
                }
            }
        }
        let _signals = Signals(registrations);
        let mut tty = Tty::new(options)?;
        let mut pending = VecDeque::new();
        let mut paste = false;
        let mut paste_bytes = Vec::new();
        loop {
            let capacity = tty.draw(menu, options)?;
            if terminated.load(Ordering::Relaxed) {
                return Err(io::Error::other("terminated by signal"));
            }
            // Bracketed paste is always query text, including strings resembling
            // acceptance/cancel keys. Its line/control bytes are ignored.
            loop {
                if paste {
                    if pending.len() < 6 {
                        break;
                    }
                    if pending
                        .iter()
                        .take(6)
                        .copied()
                        .eq(b"\x1b[201~".iter().copied())
                    {
                        pending.drain(..6);
                        paste = false;
                        menu.insert(&paste_bytes);
                        paste_bytes.clear();
                    } else if let Some(byte) = pending.pop_front() {
                        if byte >= 32 && byte != 127 && paste_bytes.len() < MAX_QUERY {
                            paste_bytes.push(byte);
                        }
                    }
                    continue;
                }
                if pending.len() >= 6
                    && pending
                        .iter()
                        .take(6)
                        .copied()
                        .eq(b"\x1b[200~".iter().copied())
                {
                    pending.drain(..6);
                    paste = true;
                    continue;
                }
                let Some(key) = decode(&mut pending) else {
                    break;
                };
                if let Some(result) = menu.key(key, capacity) {
                    tty.restore()?;
                    return Ok(result.ok());
                }
            }
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 100_000_000,
            };
            let mut descriptors = [PollFd::new(&tty.file, PollFlags::IN)];
            match poll(&mut descriptors, Some(&timeout)) {
                Ok(0) | Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
            if descriptors[0]
                .revents()
                .intersects(PollFlags::ERR | PollFlags::NVAL)
            {
                return Err(io::Error::other("terminal disconnected"));
            }
            let mut buffer = [0u8; 4096];
            let count = tty.file.read(&mut buffer)?;
            if count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "terminal disconnected",
                ));
            }
            pending.extend(&buffer[..count]);
            if pending.len() > 8192 && !paste {
                pending.clear();
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn labels_escape_terminal_controls() {
            assert!(!label(b"a\x1b[2J\n\xff", 50).as_bytes().contains(&27));
            assert!(label(b"a\x1b[2J", 50).contains("\\u{1b}"));
        }
        #[test]
        fn double_escape_and_navigation_sequences() {
            let mut bytes = VecDeque::from(b"\x1b\x1b\x1b[6~\x1bb".to_vec());
            assert!(matches!(decode(&mut bytes), Some(Key::Cancel)));
            assert!(matches!(decode(&mut bytes), Some(Key::PageDown)));
            assert!(matches!(decode(&mut bytes), Some(Key::WordBack)));
        }
    }
}

fn run(options: &Options) -> io::Result<Option<Vec<u8>>> {
    let items = read_candidates(io::stdin().lock())?;
    let mut menu = Menu::new(items, options);
    #[cfg(all(unix, feature = "terminal"))]
    {
        interactive::run(&mut menu, options)
    }
    #[cfg(not(all(unix, feature = "terminal")))]
    {
        let _ = &mut menu;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "interactive menu requires Unix and the terminal Cargo feature",
        ))
    }
}

fn main() -> std::process::ExitCode {
    match parse(std::env::args_os().skip(1)) {
        Ok(Parsed::Version) => {
            println!("vis-menu {} (Rust)", env!("CARGO_PKG_VERSION"));
            std::process::ExitCode::SUCCESS
        }
        Ok(Parsed::Run(options)) => match run(&options) {
            Ok(Some(item)) => match io::stdout()
                .lock()
                .write_all(&item)
                .and_then(|()| io::stdout().lock().write_all(b"\n"))
            {
                Ok(()) => std::process::ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("vis-menu-rs: {error}");
                    std::process::ExitCode::from(2)
                }
            },
            Ok(None) => std::process::ExitCode::from(1),
            Err(error) => {
                eprintln!("vis-menu-rs: {error}");
                std::process::ExitCode::from(2)
            }
        },
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn items() -> Vec<Vec<u8>> {
        [
            b"xfoo bar".as_slice(),
            b"foo bar",
            b"foo",
            b"foobar",
            b"FOO",
            b"bar foo",
        ]
        .into_iter()
        .map(Vec::from)
        .collect()
    }
    #[test]
    fn matching_is_stable_and_ranks_exact_before_prefix_and_substring() {
        assert_eq!(rank_matches(&items(), b"foo", false), [2, 1, 3, 0, 5]);
        assert_eq!(rank_matches(&items(), b"foo", true), [2, 4, 1, 3, 0, 5]);
        assert_eq!(rank_matches(&items(), b"foo bar", false), [1, 3, 0, 5]);
        assert_eq!(rank_matches(&items(), b"   ", false), [0, 1, 2, 3, 4, 5]);
        assert_eq!(rank_matches(&items(), b"foo\tbar", false), []);
    }
    #[test]
    fn line_input_retains_empty_and_carriage_returns_and_rejects_read_errors() {
        assert_eq!(
            read_candidates(&b"a\n\n\r\nlast\0ignored"[..]).unwrap(),
            [b"a".to_vec(), Vec::new(), b"\r".to_vec(), b"last".to_vec()]
        );
        struct Unreadable;
        impl io::Read for Unreadable {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("input failed"))
            }
        }
        assert!(read_candidates(io::BufReader::new(Unreadable)).is_err());
    }
    #[test]
    fn editing_navigation_and_acceptance_preserve_menu_contract() {
        let mut menu = Menu::new(
            items(),
            &Options {
                initial: b"foo".to_vec(),
                lines: 2,
                ..Options::default()
            },
        );
        menu.paginate(2);
        menu.key(Key::Next, 2);
        assert_eq!(menu.key(Key::Enter, 2), Some(Ok(b"foo bar".to_vec())));
        menu.key(Key::Tab, 2);
        menu.key(Key::KillWord, 2);
        assert_eq!(menu.query, b"foo ");
        assert_eq!(menu.key(Key::AcceptText, 2), Some(Ok(b"foo ".to_vec())));
        assert_eq!(menu.key(Key::Cancel, 2), Some(Err(())));
    }
    #[test]
    fn numeric_cli_matches_original_suffix_and_hex_conversion() {
        assert_eq!(line_count(b"3suffix"), Ok(3));
        assert_eq!(line_count(b"0x10"), Ok(16));
        assert_eq!(line_count(b"-1"), Ok(u64::MAX));
        // The original converter recognizes 0x only at the raw string start.
        assert_eq!(line_count(b"-0x10"), Ok(0));
        assert_eq!(line_count(b"not-a-number"), Ok(0));
        assert!(line_count(b"18446744073709551616").is_err());
    }
}

//! Safe Unix terminal input and ANSI rendering.
//!
//! Raw mode disables terminal-generated signals, so Ctrl-C/Ctrl-Z are key events.
//! The guard restores termios and the primary screen on ordinary errors and Rust
//! unwinding. Uncatchable signals and process aborts cannot run `Drop`; signal
//! integration belongs to the application. Widths follow `motion`'s documented
//! approximation rather than a terminal-specific Unicode width database.

use crate::editor::{Editor, Key, Mode};
use crate::motion;
pub use crate::style::DrawOperation;
use crate::style::Style;
use crate::workspace::WindowDisplay;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::termios::{OptionalActions, Termios, tcgetattr, tcgetwinsize, tcsetattr};
use std::collections::{BTreeMap, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::ops::Range;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

const ESCAPE_DELAY: Duration = Duration::from_millis(30);
const REFRESH_DELAY: Duration = Duration::from_millis(250);
const MAX_PASTE: usize = 8 * 1024 * 1024;
const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// Original vis tiles its windows in one global horizontal or vertical layout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    Horizontal,
    Vertical,
}

/// A borrowed window snapshot. Buffers and global editor/session state remain
/// owned by the host; rendering does not clone either an Editor or its Text.
pub struct RenderView<'a> {
    pub id: usize,
    pub data: &'a [u8],
    pub cursor: usize,
    pub selections: &'a [crate::selection::Selection],
    pub anchored: &'a [bool],
    pub selections_visible: bool,
    pub primary_index: usize,
    pub mode: Mode,
    pub path: &'a Path,
    pub modified: bool,
    pub tab_width: usize,
    pub display: WindowDisplay,
    pub prompt_executing: bool,
    /// First display row to show for this frame, before cursor auto-follow.
    pub topline: Option<usize>,
    /// First display row of the anchored view line. Native `view->start_mark`
    /// keeps the visible content in place when the text changes above it.
    pub anchor: Option<usize>,
    /// Native `wrapcolumn` option: wrap the text at this column.
    pub wrapcolumn: usize,
    /// Full prompt including its `:`, `/` or `?` prefix.
    pub prompt: Option<&'a str>,
}

/// Measured visible body, using the same wrapping and viewport as rendering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewGeometry {
    pub id: usize,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    /// Body rows, excluding the status line.
    pub height: usize,
    pub total_height: usize,
    /// First displayed soft-wrapped row.
    pub top: usize,
    pub bytes: Range<usize>,
    pub lines: Range<usize>,
}

/// Host-provided typed styles. SGR is generated only from bounded values.
#[derive(Clone, Default)]
pub struct ViewDecorations {
    pub styles: BTreeMap<usize, Style>,
    pub base: Option<Style>,
    pub whitespace: Option<Style>,
    pub operations: Vec<DrawOperation>,
    pub ranges: Vec<(usize, Range<usize>)>,
    pub cells: BTreeMap<(usize, usize), usize>,
    pub status_left: Vec<u8>,
    pub status_right: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InputEvent {
    Key(Key),
    Paste(Vec<u8>),
    Resize,
    Timeout,
    Eof,
    Message(String),
}

#[derive(Default)]
struct Decoder {
    pending: VecDeque<u8>,
    paste: Option<Vec<u8>>,
    discard_paste: bool,
    discard_control: bool,
    discard_csi: bool,
}

impl Decoder {
    fn cancel(&mut self) {
        if self.paste.is_some() {
            // SIGINT cancels the edit, not the terminal's framing protocol.
            // Discard through the end marker so remaining paste bytes cannot
            // become commands after the editor has returned to normal mode.
            self.paste = Some(Vec::new());
            self.discard_paste = true;
        } else {
            *self = Self::default();
        }
    }
    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend(bytes);
    }

    fn consume(&mut self, count: usize) {
        self.pending.drain(..count);
    }

    fn starts_with(&self, bytes: &[u8]) -> bool {
        self.pending.len() >= bytes.len() && self.pending.iter().zip(bytes).all(|(a, b)| a == b)
    }

    fn is_escape(&self) -> bool {
        self.pending.front() == Some(&0x1b)
            && self.paste.is_none()
            && !self.discard_control
            && !self.discard_csi
    }

    fn next(&mut self, escape_expired: bool) -> io::Result<Option<InputEvent>> {
        if self.discard_csi {
            if let Some(end) = self
                .pending
                .iter()
                .position(|byte| (0x40..=0x7e).contains(byte))
            {
                self.consume(end + 1);
                self.discard_csi = false;
                return Ok(Some(InputEvent::Timeout));
            }
            self.pending.clear();
            return Ok(None);
        }
        if self.discard_control {
            let end = self
                .pending
                .iter()
                .position(|&byte| byte == 7)
                .map(|i| i + 1)
                .into_iter()
                .chain((1..self.pending.len()).filter_map(|i| {
                    (self.pending[i - 1] == 0x1b && self.pending[i] == b'\\').then_some(i + 1)
                }))
                .min();
            if let Some(end) = end {
                self.consume(end);
                self.discard_control = false;
                return Ok(Some(InputEvent::Timeout));
            }
            // Retain only a possible split ST terminator; discard control
            // strings incrementally rather than accumulating arbitrary input.
            let retained = usize::from(self.pending.back() == Some(&0x1b));
            self.consume(self.pending.len() - retained);
            return Ok(None);
        }
        if let Some(paste) = &mut self.paste {
            let end = (0..self.pending.len().saturating_sub(PASTE_END.len() - 1)).find(|&pos| {
                self.pending
                    .iter()
                    .skip(pos)
                    .zip(PASTE_END)
                    .all(|(a, b)| a == b)
            });
            let retained = end.unwrap_or(self.pending.len().saturating_sub(PASTE_END.len() - 1));
            let oversized = !self.discard_paste && paste.len().saturating_add(retained) > MAX_PASTE;
            if oversized {
                self.discard_paste = true;
                *paste = Vec::new();
            }
            if self.discard_paste {
                self.pending.drain(..retained);
            } else {
                paste.extend(self.pending.drain(..retained));
            }
            if end.is_some() {
                self.consume(PASTE_END.len());
                let bytes = self.paste.take().unwrap();
                let discarded = std::mem::take(&mut self.discard_paste);
                return Ok(Some(if oversized {
                    InputEvent::Message("bracketed paste exceeds 8 MiB; paste discarded".into())
                } else if discarded {
                    InputEvent::Timeout
                } else {
                    InputEvent::Paste(bytes)
                }));
            }
            if oversized {
                return Ok(Some(InputEvent::Message(
                    "bracketed paste exceeds 8 MiB; paste discarded".into(),
                )));
            }
            return Ok(None);
        }
        let Some(&first) = self.pending.front() else {
            return Ok(None);
        };
        if first == 0x1b {
            if self.starts_with(PASTE_START) {
                self.consume(PASTE_START.len());
                self.paste = Some(Vec::new());
                return self.next(false);
            }
            if self.pending.len() == 1 {
                if !escape_expired {
                    return Ok(None);
                }
                self.consume(1);
                return Ok(Some(InputEvent::Key(Key::Escape)));
            }
            let second = self.pending[1];
            if matches!(second, b']' | b'P' | b'^' | b'_') {
                self.consume(2);
                self.discard_control = true;
                return self.next(false);
            }
            if second == b'[' || second == b'O' {
                if let Some(end) =
                    (2..self.pending.len()).find(|&i| (0x40..=0x7e).contains(&self.pending[i]))
                {
                    let final_byte = self.pending[end];
                    let parameters: Vec<_> =
                        self.pending.iter().skip(2).take(end - 2).copied().collect();
                    let plain = parameters.is_empty() || parameters == b"1";
                    let key = match final_byte {
                        b'A' if plain => Some(Key::Up),
                        b'B' if plain => Some(Key::Down),
                        b'C' if parameters == b"1;2" => Some(Key::ShiftRight),
                        b'D' if parameters == b"1;2" => Some(Key::ShiftLeft),
                        b'C' if plain => Some(Key::Right),
                        b'D' if plain => Some(Key::Left),
                        b'H' if plain => Some(Key::Home),
                        b'F' if plain => Some(Key::End),
                        b'Z' if plain || parameters == b"1;2" => Some(Key::BackTab),
                        b'P'..=b'S' if plain => Some(Key::Function(final_byte - b'P' + 1)),
                        b'~' => match parameters.as_slice() {
                            b"1" | b"7" => Some(Key::Home),
                            b"4" | b"8" => Some(Key::End),
                            b"2" => Some(Key::Insert),
                            b"3" => Some(Key::Delete),
                            b"5" => Some(Key::PageUp),
                            b"6" => Some(Key::PageDown),
                            b"5;2" => Some(Key::ShiftPageUp),
                            b"6;2" => Some(Key::ShiftPageDown),
                            b"11" => Some(Key::Function(1)),
                            b"12" => Some(Key::Function(2)),
                            b"13" => Some(Key::Function(3)),
                            b"14" => Some(Key::Function(4)),
                            b"15" => Some(Key::Function(5)),
                            b"17" => Some(Key::Function(6)),
                            b"18" => Some(Key::Function(7)),
                            b"19" => Some(Key::Function(8)),
                            b"20" => Some(Key::Function(9)),
                            b"21" => Some(Key::Function(10)),
                            b"23" => Some(Key::Function(11)),
                            b"24" => Some(Key::Function(12)),
                            b"25" => Some(Key::Function(13)),
                            b"26" => Some(Key::Function(14)),
                            b"28" => Some(Key::Function(15)),
                            b"29" => Some(Key::Function(16)),
                            b"31" => Some(Key::Function(17)),
                            b"32" => Some(Key::Function(18)),
                            b"33" => Some(Key::Function(19)),
                            b"34" => Some(Key::Function(20)),
                            _ => None,
                        },
                        _ => None,
                    };
                    self.consume(end + 1);
                    // Unsupported terminal escape sequences must never leak
                    // their constituent bytes into normal-mode commands.
                    return Ok(Some(key.map_or_else(
                        || InputEvent::Message("unsupported terminal key sequence".into()),
                        InputEvent::Key,
                    )));
                }
                if escape_expired {
                    self.pending.clear();
                    return Ok(Some(InputEvent::Key(Key::Escape)));
                }
                if self.pending.len() > 256 {
                    self.consume(2);
                    self.discard_csi = true;
                    return self.next(false);
                }
                return Ok(None);
            }
            if second == 27 {
                self.consume(1);
                return Ok(Some(InputEvent::Key(Key::Escape)));
            }
            if second < 32 || second == 127 {
                self.consume(2);
                let control = match second {
                    0 => '@',
                    1..=26 => char::from(b'a' + second - 1),
                    28..=31 => char::from(b'\\' + second - 28),
                    127 => '?',
                    _ => unreachable!("ESC handled separately"),
                };
                return Ok(Some(InputEvent::Key(Key::AltCtrl(control))));
            }
            let length = match second {
                0..=0x7f => 1,
                0xc2..=0xdf => 2,
                0xe0..=0xef => 3,
                0xf0..=0xf4 => 4,
                _ => 1,
            };
            if self.pending.len() < length + 1 {
                if !escape_expired {
                    return Ok(None);
                }
                self.consume(1);
                return Ok(Some(InputEvent::Key(Key::Escape)));
            }
            let bytes: Vec<_> = self.pending.iter().skip(1).take(length).copied().collect();
            let (character, consumed) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| text.chars().next())
                .map_or(('\u{fffd}', 1), |character| (character, length));
            self.consume(1 + consumed);
            return Ok(Some(InputEvent::Key(Key::Alt(character))));
        }
        let key = match first {
            b'\r' => Key::Enter,
            b'\t' => Key::Tab,
            0x08 | 0x7f => Key::Backspace,
            0 => Key::Ctrl('@'),
            1..=26 => Key::Ctrl(char::from(b'a' + first - 1)),
            28..=31 => Key::Ctrl(char::from(b'\\' + first - 28)),
            32..=126 => Key::Char(char::from(first)),
            _ => {
                let length = match first {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf4 => 4,
                    _ => 1,
                };
                if self.pending.len() < length {
                    return Ok(None);
                }
                let bytes: Vec<_> = self.pending.iter().take(length).copied().collect();
                if let Ok(value) = std::str::from_utf8(&bytes) {
                    let character = value.chars().next().unwrap();
                    self.consume(length);
                    return Ok(Some(InputEvent::Key(Key::Char(character))));
                }
                Key::Char('\u{fffd}')
            }
        };
        self.consume(1);
        Ok(Some(InputEvent::Key(key)))
    }
}

pub struct Terminal {
    tty: File,
    original: Termios,
    active: bool,
    size: (usize, usize),
    decoder: Decoder,
    escape_since: Option<Instant>,
    viewports: BTreeMap<usize, Viewport>,
    /// Native `escdelay` option: how long an incomplete escape sequence is
    /// collected before it is read as a plain escape key.
    escape_delay: Duration,
}

impl Terminal {
    pub fn new() -> io::Result<Self> {
        // macOS poll does not support the /dev/tty alias. Resolve the actual
        // terminal device first, without unsafe select or numeric descriptors.
        let name = if io::stdin().is_terminal() {
            rustix::termios::ttyname(io::stdin(), Vec::new())?
        } else if io::stdout().is_terminal() {
            rustix::termios::ttyname(io::stdout(), Vec::new())?
        } else if io::stderr().is_terminal() {
            rustix::termios::ttyname(io::stderr(), Vec::new())?
        } else {
            // Pipelines and command substitution may redirect every standard
            // stream while the process still has a controlling terminal.
            let controlling = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
            rustix::termios::ttyname(&controlling, Vec::new())?
        };
        let tty = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as _)
            .open(Path::new(std::ffi::OsStr::from_bytes(name.to_bytes())))?;
        let original = tcgetattr(&tty)?;
        let mut raw = original.clone();
        raw.make_raw();
        let size = terminal_size(&tty)?;
        tcsetattr(&tty, OptionalActions::Now, &raw)?;
        // Construct the guard before entering the alternate screen, so even a
        // failed initial write restores the already-changed terminal settings.
        let mut terminal = Self {
            tty,
            original,
            active: true,
            size,
            decoder: Decoder::default(),
            escape_since: None,
            viewports: BTreeMap::new(),
            escape_delay: ESCAPE_DELAY,
        };
        terminal
            .tty
            .write_all(b"\x1b[?1049h\x1b[?2004h\x1b[?25h\x1b[2J\x1b[H")?;
        terminal.tty.flush()?;
        Ok(terminal)
    }

    pub fn dimensions(&self) -> (usize, usize) {
        self.size
    }

    /// Apply the native `escdelay` option.
    pub fn set_escape_delay(&mut self, delay: Duration) {
        self.escape_delay = delay;
    }

    /// Cancel an incomplete key or paste when the application receives SIGINT.
    pub fn cancel_input(&mut self) {
        self.decoder.cancel();
        self.escape_since = None;
    }

    pub fn read_event(&mut self) -> io::Result<InputEvent> {
        let size = terminal_size(&self.tty)?;
        if size != self.size {
            self.size = size;
            return Ok(InputEvent::Resize);
        }
        loop {
            if self.decoder.is_escape() {
                self.escape_since.get_or_insert_with(Instant::now);
            } else {
                self.escape_since = None;
            }
            let elapsed = self.escape_since.map(|instant| instant.elapsed());
            if let Some(event) = self
                .decoder
                .next(elapsed.is_some_and(|time| time >= self.escape_delay))?
            {
                self.escape_since = None;
                return Ok(event);
            }
            let delay = elapsed.map_or(REFRESH_DELAY, |elapsed| {
                self.escape_delay.saturating_sub(elapsed)
            });
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: delay.as_nanos() as _,
            };
            let mut descriptors = [PollFd::new(&self.tty, PollFlags::IN)];
            match poll(&mut descriptors, Some(&timeout)) {
                Ok(0) if self.decoder.is_escape() => continue,
                Ok(0) => return Ok(InputEvent::Timeout),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => return Ok(InputEvent::Timeout),
                Err(error) => return Err(error.into()),
            }
            if descriptors[0]
                .revents()
                .intersects(PollFlags::ERR | PollFlags::NVAL)
            {
                return Err(io::Error::other("terminal input is unavailable"));
            }
            let mut bytes = [0u8; 4096];
            match self.tty.read(&mut bytes) {
                Ok(0) if self.decoder.paste.is_some() => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unterminated bracketed paste",
                    ));
                }
                Ok(0) => return Ok(InputEvent::Eof),
                Ok(count) => self.decoder.push(&bytes[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    return Ok(InputEvent::Timeout);
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub fn render(&mut self, editor: &Editor, path: &Path, message: &str) -> io::Result<()> {
        let data = editor.text.to_vec();
        let prompt = matches!(editor.mode, Mode::Command)
            .then(|| format!("{}{}", editor.prompt_prefix(), editor.command));
        let anchored: Vec<_> = editor
            .selection_ids()
            .iter()
            .map(|id| editor.selection_anchored(*id).unwrap_or(false))
            .collect();
        self.render_views(
            &[RenderView {
                id: 0,
                data: &data,
                cursor: editor.cursor,
                selections: editor.selections(),
                anchored: &anchored,
                selections_visible: true,
                primary_index: editor.primary_index(),
                mode: editor.mode,
                path,
                modified: editor.text.modified(),
                tab_width: editor.tab_width,
                display: WindowDisplay::default(),
                prompt_executing: false,
                topline: None,
                anchor: None,
                wrapcolumn: 0,
                prompt: prompt.as_deref(),
            }],
            0,
            Layout::Horizontal,
            message,
        )
        .map(|_| ())
    }

    /// Draw every visible window using independent stable-id viewports, then
    /// place the hardware cursor in the active window. Tiny terminals retain
    /// the active window even when every open window cannot fit simultaneously.
    pub fn render_views(
        &mut self,
        views: &[RenderView<'_>],
        active_id: usize,
        layout: Layout,
        message: &str,
    ) -> io::Result<Vec<ViewGeometry>> {
        self.size = terminal_size(&self.tty)?;
        let frame = compose_views(
            &mut self.viewports,
            views,
            active_id,
            layout,
            message,
            self.size,
        )?;
        self.tty.write_all(frame.as_bytes())?;
        self.tty.flush()?;
        Ok(self.geometries(views))
    }

    pub fn measure_views(
        &mut self,
        views: &[RenderView<'_>],
        active_id: usize,
        layout: Layout,
    ) -> io::Result<Vec<ViewGeometry>> {
        self.measure_views_with_message(views, active_id, layout, "")
    }

    pub fn measure_views_with_message(
        &mut self,
        views: &[RenderView<'_>],
        active_id: usize,
        layout: Layout,
        message: &str,
    ) -> io::Result<Vec<ViewGeometry>> {
        self.size = terminal_size(&self.tty)?;
        compose_views(
            &mut self.viewports,
            views,
            active_id,
            layout,
            message,
            self.size,
        )?;
        Ok(self.geometries(views))
    }

    fn geometries(&self, views: &[RenderView<'_>]) -> Vec<ViewGeometry> {
        views
            .iter()
            .map(|view| {
                self.viewports
                    .get(&view.id)
                    .and_then(|viewport| viewport.geometry.clone())
                    .unwrap_or_else(|| {
                        let cursor = view.cursor.min(view.data.len());
                        let line = 1 + view.data[..cursor]
                            .iter()
                            .filter(|&&byte| byte == b'\n')
                            .count();
                        ViewGeometry {
                            id: view.id,
                            x: 0,
                            y: 0,
                            width: 0,
                            height: 0,
                            total_height: 0,
                            top: 0,
                            bytes: cursor..cursor,
                            lines: line..line,
                        }
                    })
            })
            .collect()
    }

    pub fn render_views_styled(
        &mut self,
        views: &[RenderView<'_>],
        active_id: usize,
        layout: Layout,
        message: &str,
        decorations: &BTreeMap<usize, ViewDecorations>,
    ) -> io::Result<Vec<ViewGeometry>> {
        self.size = terminal_size(&self.tty)?;
        let frame = compose_views_decorated(
            &mut self.viewports,
            views,
            active_id,
            layout,
            message,
            self.size,
            decorations,
        )?;
        self.tty.write_all(frame.as_bytes())?;
        self.tty.flush()?;
        Ok(self.geometries(views))
    }

    /// Re-enter the editor screen after suspend or a fullscreen child. Keep
    /// already-decoded input and viewports so typeahead cannot disappear.
    pub fn resume(&mut self) -> io::Result<()> {
        if self.active {
            return Ok(());
        }
        let mut raw = self.original.clone();
        raw.make_raw();
        tcsetattr(&self.tty, OptionalActions::Now, &raw)?;
        self.active = true;
        self.escape_since = None;
        self.size = terminal_size(&self.tty)?;
        self.tty
            .write_all(b"\x1b[?1049h\x1b[?2004h\x1b[?25h\x1b[2J\x1b[H")?;
        self.tty.flush()
    }

    /// Restore terminal settings even if restoring screen output fails.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.active {
            return Ok(());
        }
        let output = self
            .tty
            .write_all(b"\x1b[0m\x1b[?25h\x1b[?2004l\x1b[?1049l")
            .and_then(|()| self.tty.flush());
        let settings =
            tcsetattr(&self.tty, OptionalActions::Now, &self.original).map_err(io::Error::from);
        if settings.is_ok() {
            self.active = false;
        }
        output.and(settings)
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

fn terminal_size(tty: &File) -> io::Result<(usize, usize)> {
    let size = tcgetwinsize(tty)?;
    Ok((
        usize::from(size.ws_col).max(1),
        usize::from(size.ws_row).max(1),
    ))
}

/// Render a single source character without allowing terminal control sequences.
/// Returns rendered text, display columns and consumed source bytes.
fn display_token(data: &[u8], pos: usize, column: usize) -> (String, usize, usize) {
    let byte = data[pos];
    if byte == b'\t' {
        let width = 8 - column % 8;
        return (" ".repeat(width), width, 1);
    }
    if byte < 32 || byte == 127 {
        return (
            format!(
                "^{}",
                char::from(if byte == 127 { b'?' } else { byte + 64 })
            ),
            2,
            1,
        );
    }
    let length = match byte {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => 1,
    };
    if let Some(bytes) = data.get(pos..pos.saturating_add(length)) {
        if let Ok(text) = std::str::from_utf8(bytes) {
            let c = text.chars().next().unwrap();
            if c.is_control() {
                let escaped = format!("\\u{{{:x}}}", c as u32);
                let width = escaped.len();
                return (escaped, width, length);
            }
            let width = motion::codepoint_width(data, pos);
            if width == 0 && column == 0 {
                return (format!("\u{25cc}{text}"), 1, length);
            }
            return (text.to_owned(), width, length);
        }
    }
    (format!("<{byte:02X}>"), 4, 1)
}

fn source_column(data: &[u8], start: usize, end: usize) -> usize {
    let mut pos = start;
    let mut column = 0;
    while pos < end.min(data.len()) {
        let (_, width, len) = display_token(data, pos, column);
        if pos + len > end {
            break;
        }
        column += width;
        pos += len;
    }
    column
}

#[cfg(test)]
fn clipped_line(
    data: &[u8],
    range: Range<usize>,
    left: usize,
    columns: usize,
    selections: &[Range<usize>],
) -> String {
    render_line(data, range, left, columns, selections).text
}

struct RenderedLine {
    text: String,
    width: usize,
}

fn render_line(
    data: &[u8],
    range: Range<usize>,
    left: usize,
    columns: usize,
    selections: &[Range<usize>],
) -> RenderedLine {
    let mut output = String::new();
    let mut rendered_width = 0;
    let mut pos = range.start;
    let mut column = 0;
    let mut selected = false;
    let mut visible_base = false;
    while pos < range.end {
        let (text, width, len) = display_token(data, pos, column);
        let next = column + width;
        if column >= left + columns {
            break;
        }
        if next > left || (width == 0 && column >= left) {
            if width == 0 && !visible_base {
                pos += len;
                continue;
            }
            let highlight = selections.iter().any(|range| {
                (pos < range.end && pos + len > range.start)
                    || (range.is_empty() && pos <= range.start && range.start < pos + len)
            });
            if highlight != selected {
                output.push_str(if highlight { "\x1b[7m" } else { "\x1b[0m" });
                selected = highlight;
            }
            if column >= left && next <= left + columns {
                output.push_str(&text);
                rendered_width += width;
                visible_base |= width > 0;
            } else {
                // Do not emit half a wide character, control spelling or tab.
                let visible = next.min(left + columns).saturating_sub(column.max(left));
                output.push_str(&" ".repeat(visible));
                rendered_width += visible;
                visible_base = false;
            }
        }
        column = next;
        pos += len;
    }
    if selected {
        output.push_str("\x1b[0m");
    }
    // Newlines and EOF occupy a visible cursor cell even though they have no
    // printable source bytes. This also makes empty EOF selections observable.
    let virtual_selected = selections.iter().any(|selection| {
        (selection.is_empty() && selection.start == range.end)
            || (data.get(range.end) == Some(&b'\n')
                && selection.start <= range.end
                && range.end < selection.end)
    });
    if virtual_selected && column >= left && column < left.saturating_add(columns) {
        let padding = (column - left).saturating_sub(rendered_width);
        output.push_str(&" ".repeat(padding));
        output.push_str("\x1b[7m \x1b[0m");
        rendered_width += padding + 1;
    }
    RenderedLine {
        text: output,
        width: rendered_width,
    }
}

#[derive(Default)]
struct Viewport {
    /// First displayed soft-wrapped row, measured from the start of the buffer.
    top: usize,
    geometry: Option<ViewGeometry>,
    /// Width of the native line number sidebar of the previous frame. The
    /// native editor resizes the view when the sidebar width changes, so the
    /// width follows the visible line numbers.
    sidebar_width: usize,
}

#[derive(Clone, Copy)]
struct Region {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

struct PanelFrame {
    text: String,
    cursor: (usize, usize),
}

/// Re-exported for compatibility: coordinate requests share the core layout.
pub use crate::view::screen_position;
use crate::view::{CellKind, walk_cells};

/// Zero-based soft-wrapped coordinates for a cursor position.
fn wrapped_cursor(data: &[u8], cursor: usize, columns: usize, tab_width: usize) -> (usize, usize) {
    screen_position(data, cursor, columns.max(1), tab_width)
}

#[cfg(test)]
fn wrapped_rows(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    visible: Range<usize>,
    selections: &[Range<usize>],
) -> Vec<RenderedLine> {
    wrapped_rows_decorated(data, columns, tab_width, visible, selections, None, true)
}

fn wrapped_rows_decorated(
    data: &[u8],
    columns: usize,
    tab_width: usize,
    visible: Range<usize>,
    selections: &[Range<usize>],
    decorations: Option<&ViewDecorations>,
    show_eof: bool,
) -> Vec<RenderedLine> {
    let mut lines: Vec<_> = (visible.clone())
        .map(|_| RenderedLine {
            text: String::new(),
            width: 0,
        })
        .collect();
    let mut selected = vec![false; lines.len()];
    let mut sequences = vec![String::new(); lines.len()];
    let mut eof_row = None;
    walk_cells(data, columns, tab_width, |cell| {
        if matches!(cell.kind, CellKind::Eof) {
            eof_row = Some(cell.row);
        }
        if cell.row >= visible.end {
            return false;
        }
        if cell.row < visible.start {
            return true;
        }
        let index = cell.row - visible.start;
        let line = &mut lines[index];
        let highlight = selections.iter().any(|range| {
            (cell.pos < range.end && cell.pos + cell.len > range.start)
                || (range.is_empty()
                    && (range.start == cell.pos
                        || (cell.pos < range.start && range.start < cell.pos + cell.len)))
        });
        if let Some(styles) = decorations {
            let mut style = resolved_style(Some(styles), 0);
            for (id, range) in &styles.ranges {
                // C advances to the first cell whose source position reaches
                // start. A start inside a UTF-8 scalar skips that scalar; an
                // inclusive end has already been converted by the Lua bridge.
                let overlaps = range.contains(&cell.pos);
                if overlaps {
                    style = style.merge(resolved_style(Some(styles), *id));
                }
            }
            if let Some(id) = styles.cells.get(&(cell.column, index)) {
                style = style.merge(resolved_style(Some(styles), *id));
            }
            if highlight {
                style = style.merge(resolved_style(Some(styles), 4));
            }
            let sequence = style.to_sgr();
            if sequence != sequences[index] {
                line.text.push_str(&sequence);
                sequences[index] = sequence;
            }
            selected[index] = true;
        } else if highlight != selected[index] {
            line.text
                .push_str(if highlight { "\x1b[7m" } else { "\x1b[0m" });
            selected[index] = highlight;
        }
        match cell.kind {
            CellKind::Source => line
                .text
                .push_str(&display_token(data, cell.pos, cell.column).0),
            CellKind::Replacement => line.text.push('?'),
            CellKind::Space | CellKind::Eof => line.text.push(' '),
        }
        line.width += cell.width;
        true
    });
    for (index, line) in lines.iter_mut().enumerate() {
        if selected[index] {
            line.text.push_str("\x1b[0m");
        }
        if show_eof && eof_row.is_some_and(|row| visible.start + index > row) {
            if decorations.is_some() {
                line.text.push_str(
                    &resolved_style(decorations, 0)
                        .merge(resolved_style(decorations, 12))
                        .to_sgr(),
                );
            }
            line.text.push('~');
            line.width = 1;
        }
        if let Some(styles) = decorations {
            append_styled_padding(
                &mut line.text,
                line.width..columns,
                index,
                resolved_style(decorations, 0),
                styles,
            );
            line.width = columns;
            line.text.push_str("\x1b[0m");
        }
    }
    lines
}

fn highlights(view: &RenderView<'_>, _focused: bool) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    if !view.selections_visible {
        return ranges;
    }
    for (index, selection) in view.selections.iter().enumerate() {
        let anchored = view.anchored.get(index).copied().unwrap_or(false);
        if anchored {
            if let Ok(range) = selection.range(view.data) {
                ranges.push(range);
            }
        }
    }
    ranges
}

#[derive(Default)]
struct OverlayContext {
    cursors: Vec<(usize, bool)>,
    match_brackets: bool,
    native_view: bool,
}

fn compose_views(
    viewports: &mut BTreeMap<usize, Viewport>,
    views: &[RenderView<'_>],
    active_id: usize,
    layout: Layout,
    message: &str,
    size: (usize, usize),
) -> io::Result<String> {
    compose_views_decorated(
        viewports,
        views,
        active_id,
        layout,
        message,
        size,
        &BTreeMap::new(),
    )
}

// One-line windows occupy full-width rows below the ordinary tiled views.
// Keep the active view visible when the terminal cannot fit every window.
fn visible_indices(indices: &[usize], active: usize, maximum: usize) -> &[usize] {
    let count = indices.len().min(maximum);
    if count == 0 {
        return &[];
    }
    let focused = indices
        .iter()
        .position(|&index| index == active)
        .unwrap_or(indices.len() - 1);
    let first = focused.saturating_sub(count - 1).min(indices.len() - count);
    &indices[first..first + count]
}

type ViewArrangement = (Vec<(usize, Region)>, Vec<(usize, usize)>);

fn view_regions(
    views: &[RenderView<'_>],
    active: usize,
    layout: Layout,
    columns: usize,
    rows: usize,
) -> ViewArrangement {
    let (oneline, regular): (Vec<_>, Vec<_>) =
        (0..views.len()).partition(|&index| views[index].display.oneline);
    let reserved = usize::from(!regular.is_empty() && rows > 1);
    let oneline = visible_indices(&oneline, active, rows.saturating_sub(reserved));
    let regular_height = rows - oneline.len();
    let maximum = if regular_height == 0 {
        0
    } else {
        match layout {
            Layout::Horizontal => regular_height,
            Layout::Vertical => columns.saturating_add(1) / 2,
        }
    };
    let regular = visible_indices(&regular, active, maximum);
    let mut regions = Vec::new();
    let mut separators = Vec::new();
    let mut origin = 0;
    for (position, &index) in regular.iter().enumerate() {
        let last = position + 1 == regular.len();
        let region = match layout {
            Layout::Horizontal => Region {
                x: 0,
                y: origin,
                width: columns,
                height: if last {
                    regular_height - origin
                } else {
                    regular_height / regular.len()
                },
            },
            Layout::Vertical => Region {
                x: origin,
                y: 0,
                width: if last {
                    columns - origin
                } else {
                    (columns / regular.len()).saturating_sub(1).max(1)
                },
                height: regular_height,
            },
        };
        match layout {
            Layout::Horizontal => origin += region.height,
            Layout::Vertical => {
                origin += region.width;
                if !last {
                    separators.push((origin, regular_height));
                    origin += 1;
                }
            }
        }
        regions.push((index, region));
    }
    for (position, &index) in oneline.iter().enumerate() {
        regions.push((
            index,
            Region {
                x: 0,
                y: regular_height + position,
                width: columns,
                height: 1,
            },
        ));
    }
    (regions, separators)
}

fn compose_views_decorated(
    viewports: &mut BTreeMap<usize, Viewport>,
    views: &[RenderView<'_>],
    active_id: usize,
    layout: Layout,
    message: &str,
    size: (usize, usize),
    decorations: &BTreeMap<usize, ViewDecorations>,
) -> io::Result<String> {
    let active_index = views
        .iter()
        .position(|view| view.id == active_id)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "active render view is missing")
        })?;
    let mut ids = std::collections::BTreeSet::new();
    if views.iter().any(|view| !ids.insert(view.id)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "duplicate render view id",
        ));
    }
    viewports.retain(|id, _| ids.contains(id));
    let (columns, rows) = (size.0.max(1), size.1.max(1));
    let info_row = !message.is_empty();
    let usable_rows = rows - usize::from(info_row);
    let (regions, separators) = view_regions(views, active_index, layout, columns, usable_rows);
    for (id, viewport) in viewports.iter_mut() {
        if !regions.iter().any(|(index, _)| views[*index].id == *id) {
            viewport.geometry = None;
        }
    }
    let mut frame = String::from("\x1b[?25l\x1b[0m");
    // Paint bounded rows instead of EL/ED, which would erase neighboring panels
    // when a split is redrawn. This also clears cells of closed/hidden views.
    for row in 1..=rows {
        frame.push_str(&format!("\x1b[{row};1H{}", " ".repeat(columns)));
    }
    let mut cursor = (rows, 1);
    for (index, region) in regions {
        let view = &views[index];
        let focused = view.id == active_id;
        let position = view.cursor.min(view.data.len());
        let status = format!(
            " {}{} -- {} -- {}:{}",
            view.mode.name(),
            if view.modified { " [+]" } else { "" },
            view.path.display(),
            1 + view.data[..position]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count(),
            1 + position - motion::line_begin(view.data, position)
        );
        let viewport = viewports.entry(view.id).or_default();
        let overlay = OverlayContext {
            native_view: !view.selections.is_empty(),
            cursors: if focused {
                view.selections
                    .iter()
                    .enumerate()
                    .map(|(index, selection)| {
                        (
                            selection.cursor.min(view.data.len()),
                            index == view.primary_index,
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            },
            match_brackets: focused && !matches!(view.mode, Mode::Visual | Mode::VisualLine),
        };
        let panel = viewport.panel_decorated(
            view.data,
            view.cursor,
            &highlights(view, focused),
            &status,
            if focused { view.prompt } else { None },
            region,
            focused,
            view.tab_width,
            decorations.get(&view.id),
            view.display,
            &overlay,
            view.topline,
            view.anchor,
        );
        let body = region
            .height
            .saturating_sub(usize::from(view.display.statusbar));
        let mut start = None;
        let mut end = 0;
        walk_cells(view.data, region.width, view.tab_width, |cell| {
            if cell.row >= viewport.top.saturating_add(body) {
                return false;
            }
            if cell.row >= viewport.top {
                start.get_or_insert(cell.pos);
                end = cell.pos + cell.len;
            }
            true
        });
        let start = start.unwrap_or(view.cursor.min(view.data.len()));
        let first_line = 1 + view.data[..start].iter().filter(|&&b| b == b'\n').count();
        let last_line = first_line
            + view.data[start..end.max(start)]
                .iter()
                .filter(|&&b| b == b'\n')
                .count()
            + usize::from(end > start && view.data.get(end - 1) != Some(&b'\n'));
        viewport.geometry = Some(ViewGeometry {
            id: view.id,
            x: region.x,
            y: region.y,
            width: region.width,
            height: body,
            total_height: region.height,
            top: viewport.top,
            bytes: start..end.max(start),
            lines: first_line..last_line,
        });
        frame.push_str(&panel.text);
        if focused {
            cursor = panel.cursor;
        }
    }
    for (column, height) in separators {
        for row in 1..=height {
            frame.push_str(&format!("\x1b[{row};{}H", column + 1));
            if let Some(style) = decorations.get(&active_id) {
                frame.push_str(
                    &resolved_style(Some(style), 0)
                        .merge(resolved_style(Some(style), 10))
                        .to_sgr(),
                );
            }
            frame.push('|');
        }
    }
    if info_row {
        let label = message.as_bytes();
        let rendered = render_line(label, 0..label.len(), 0, columns, &[]);
        frame.push_str(&format!("\x1b[{rows};1H\x1b[0m"));
        if let Some(style) = decorations.get(&active_id) {
            frame.push_str(
                &resolved_style(Some(style), 0)
                    .merge(resolved_style(Some(style), 11))
                    .to_sgr(),
            );
        }
        frame.push_str(&rendered.text);
        frame.push_str(&" ".repeat(columns.saturating_sub(rendered.width)));
        frame.push_str("\x1b[0m");
    }
    if views[active_index].prompt_executing {
        cursor.0 = rows;
    }
    frame.push_str(&format!("\x1b[{};{}H\x1b[?25h", cursor.0, cursor.1));
    Ok(frame)
}

impl Viewport {
    #[cfg(test)]
    fn frame(
        &mut self,
        data: &[u8],
        cursor: usize,
        selections: &[Range<usize>],
        status: &str,
        prompt: Option<&str>,
        size: (usize, usize),
    ) -> String {
        let panel = self.panel(
            data,
            cursor,
            selections,
            status,
            prompt,
            Region {
                x: 0,
                y: 0,
                width: size.0,
                height: size.1,
            },
            true,
            8,
        );
        format!(
            "\x1b[?25l\x1b[0m{}\x1b[{};{}H\x1b[?25h",
            panel.text, panel.cursor.0, panel.cursor.1
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn panel(
        &mut self,
        data: &[u8],
        cursor: usize,
        selections: &[Range<usize>],
        status: &str,
        prompt: Option<&str>,
        region: Region,
        focused: bool,
        tab_width: usize,
    ) -> PanelFrame {
        self.panel_decorated(
            data,
            cursor,
            selections,
            status,
            prompt,
            region,
            focused,
            tab_width,
            None,
            WindowDisplay::default(),
            &OverlayContext::default(),
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn panel_decorated(
        &mut self,
        data: &[u8],
        cursor: usize,
        selections: &[Range<usize>],
        status: &str,
        prompt: Option<&str>,
        region: Region,
        focused: bool,
        tab_width: usize,
        decorations: Option<&ViewDecorations>,
        display: WindowDisplay,
        overlay: &OverlayContext,
        requested_top: Option<usize>,
        anchor: Option<usize>,
    ) -> PanelFrame {
        let (columns, rows) = (region.width, region.height);
        let body = rows.saturating_sub(usize::from(display.statusbar));
        // The anchored line is the first visible row of the previous frame,
        // tracked by a text mark so that edits above the view keep the
        // visible content in place.
        self.top = anchor.unwrap_or(self.top);
        if let Some(previous) = &self.geometry {
            // Growing a window exposes preceding rows, including the history
            // above an expanded one-line prompt, while preserving its cursor.
            self.top = self
                .top
                .saturating_sub(body.saturating_sub(previous.height));
        }
        // Native line number sidebar: its width follows the visible line
        // numbers and the body is laid out in the remaining columns.
        let layout_columns = columns.saturating_sub(self.sidebar_width).max(1);
        let sidebar = sidebar_columns(data, self.top, body, layout_columns, tab_width, &display);
        self.sidebar_width = sidebar;
        let text_columns = columns.saturating_sub(sidebar).max(1);
        // Native `view_max_text_width`: the text wraps at the smaller of the
        // window width and the `wrapcolumn` option.
        let wrap_columns = if display.wrapcolumn > 0 {
            display.wrapcolumn.min(text_columns)
        } else {
            text_columns
        };
        let (cursor_row, cursor_column) = wrapped_cursor(data, cursor, wrap_columns, tab_width);
        let cursor_line =
            crate::view::line_number_at_row(data, cursor_row, text_columns, tab_width).unwrap_or(1);
        let numbers = line_numbers(
            data,
            self.top,
            body,
            text_columns,
            tab_width,
            &display,
            cursor_line,
            sidebar,
        );
        if let Some(top) = requested_top {
            // Scroll actions place the cursor where the user will see it; do
            // not pull the view back, only clamp to the last row that exists.
            self.top = top.min(crate::view::last_row(data, columns, tab_width));
        } else {
            if cursor_row < self.top {
                self.top = cursor_row;
            }
            if body > 0 && cursor_row >= self.top.saturating_add(body) {
                self.top = cursor_row + 1 - body;
            }
        }
        let mut fallback = ViewDecorations::default();
        if decorations.is_none() && overlay.native_view && prompt.is_none() {
            fallback.operations.push(DrawOperation::CoreOverlays {
                selection: resolved_style(None, 4),
                cursor: resolved_style(None, 1),
                primary: resolved_style(None, 2),
                eof: resolved_style(None, 12),
            });
            if display.statusbar {
                fallback.operations.push(DrawOperation::Status {
                    left: status.as_bytes().to_vec(),
                    right: Vec::new(),
                    style: resolved_style(None, if focused { 9 } else { 8 }),
                });
            }
            fallback.operations.push(DrawOperation::BodyCopy);
        }
        let decorations =
            decorations.or_else(|| (!fallback.operations.is_empty()).then_some(&fallback));
        if let Some(styles) = decorations.filter(|styles| !styles.operations.is_empty()) {
            let text = ordered_panel(
                data,
                self.top,
                selections,
                region,
                tab_width,
                display,
                styles,
                overlay,
                &numbers,
                wrap_columns,
            );
            return PanelFrame {
                text,
                cursor: (
                    region.y
                        + if body == 0 {
                            rows
                        } else {
                            cursor_row.saturating_sub(self.top).min(body - 1) + 1
                        },
                    region.x + sidebar + cursor_column.min(text_columns - 1) + 1,
                ),
            };
        }
        let mut frame = String::from("\x1b[0m");
        let visible = wrapped_rows_decorated(
            data,
            wrap_columns,
            tab_width,
            self.top..self.top + body,
            selections,
            decorations,
            display.showeof,
        );
        for (row, rendered) in visible.into_iter().enumerate() {
            frame.push_str(&format!("\x1b[{};{}H", region.y + row + 1, region.x + 1));
            if sidebar > 0 {
                if let Some(number) = numbers.number(row) {
                    let text = number.to_string();
                    let padding = sidebar.saturating_sub(1 + text.len());
                    frame.push_str(&" ".repeat(padding));
                    frame.push_str(&text);
                    frame.push(' ');
                } else {
                    frame.push_str(&" ".repeat(sidebar));
                }
            }
            frame.push_str(&rendered.text);
            frame.push_str(&" ".repeat(text_columns.saturating_sub(rendered.width)));
            frame.push_str("\x1b[0m");
        }
        if display.statusbar {
            frame.push_str(&format!(
                "\x1b[{};{}H{}",
                region.y + rows,
                region.x + 1,
                if focused { "\x1b[7m" } else { "\x1b[0m" }
            ));
            let custom = decorations
                .filter(|d| !d.status_left.is_empty() || !d.status_right.is_empty())
                .map(|d| {
                    let right =
                        render_line(&d.status_right, 0..d.status_right.len(), 0, columns, &[]);
                    let left = render_line(
                        &d.status_left,
                        0..d.status_left.len(),
                        0,
                        columns.saturating_sub(right.width),
                        &[],
                    );
                    format!(
                        "{}{}{}",
                        left.text,
                        " ".repeat(columns.saturating_sub(left.width + right.width)),
                        right.text
                    )
                });
            let label = prompt.or(custom.as_deref()).unwrap_or(status).as_bytes();
            if let Some(styles) = decorations {
                let style = resolved_style(decorations, 0)
                    .merge(resolved_style(decorations, if focused { 9 } else { 8 }));
                frame.push_str(&styled_label(label, columns, body, style, styles));
            } else {
                let rendered = render_line(label, 0..label.len(), 0, columns, &[]);
                frame.push_str(&rendered.text);
                frame.push_str(&" ".repeat(columns.saturating_sub(rendered.width)));
                frame.push_str("\x1b[0m");
            }
        }
        let (row, column) = if let Some(prompt) = prompt {
            (
                rows,
                source_column(prompt.as_bytes(), 0, prompt.len()).min(columns - 1) + 1,
            )
        } else if body == 0 {
            (rows, 1)
        } else {
            (
                cursor_row.saturating_sub(self.top).min(body - 1) + 1,
                cursor_column.min(text_columns - 1) + 1,
            )
        };
        PanelFrame {
            text: frame,
            cursor: (region.y + row, region.x + sidebar + column),
        }
    }
}

#[derive(Clone)]
struct PaintCell {
    text: String,
    width: usize,
    style: Style,
    source: Option<usize>,
    file_bytes: usize,
    in_line: bool,
    eof_symbol: bool,
}

fn paint_text(cells: &mut [PaintCell], mut column: usize, text: &[u8], style: Style) {
    let mut position = 0;
    while position < text.len() {
        let (token, width, length) = display_token(text, position, column);
        if width == 0 || column + width > cells.len() {
            break;
        }
        cells[column].text = token;
        cells[column].width = width;
        cells[column].style = style;
        for next in 1..width {
            cells[column + next].text.clear();
            cells[column + next].width = 0;
            cells[column + next].style = style;
        }
        column += width;
        position += length;
    }
}

/// Port vis_win_style's traversal rather than treating continuation cells as
/// separate source characters. In particular an inclusive endpoint on a tab
/// styles its first cell, and EOF cursor padding is outside the line's width.
fn paint_range(
    cells: &mut [PaintCell],
    width: usize,
    mut position: usize,
    range: &Range<usize>,
    style: Style,
) {
    if range.is_empty() {
        return;
    }
    let height = cells.len() / width;
    let metadata: Vec<_> = cells
        .chunks(width)
        .map(|row| {
            (
                row.iter().map(|cell| cell.file_bytes).sum::<usize>(),
                row.iter()
                    .rposition(|cell| cell.in_line)
                    .map_or(0, |column| column + 1),
            )
        })
        .collect();
    let mut row = 0;
    while row < height && position.saturating_add(metadata[row].0) <= range.start {
        position = position.saturating_add(metadata[row].0);
        row += 1;
    }
    if row == height {
        return;
    }
    let mut column = 0;
    while position < range.start && column < width {
        position += cells[row * width + column].file_bytes;
        column += 1;
    }
    while column < width && cells[row * width + column].file_bytes == 0 {
        column += 1;
    }
    loop {
        while position < range.end && column < metadata[row].1 {
            let cell = &mut cells[row * width + column];
            position += cell.file_bytes;
            cell.style = cell.style.merge(style);
            column += 1;
        }
        if position < range.end - 1 {
            while column < width {
                let cell = &mut cells[row * width + column];
                cell.style = cell.style.merge(style);
                column += 1;
            }
        }
        row += 1;
        if row == height || position >= range.end - 1 {
            break;
        }
        column = 0;
    }
}

#[allow(clippy::too_many_arguments)]
fn ordered_panel(
    data: &[u8],
    top: usize,
    selections: &[Range<usize>],
    region: Region,
    tab_width: usize,
    display: WindowDisplay,
    decorations: &ViewDecorations,
    overlay: &OverlayContext,
    numbers: &LineNumbers,
    wrap_width: usize,
) -> String {
    let width = region.width;
    let height = region.height;
    let body_height = height.saturating_sub(usize::from(display.statusbar));
    let sidebar = numbers.width;
    let text_width = width.saturating_sub(sidebar).max(1);
    let wrap_width = wrap_width.max(1).min(text_width);
    let base = decorations
        .base
        .unwrap_or_else(|| resolved_style(Some(decorations), 0));
    let whitespace = decorations
        .whitespace
        .unwrap_or_else(|| resolved_style(Some(decorations), 13));
    let blank = PaintCell {
        text: " ".into(),
        width: 1,
        style: base,
        source: None,
        file_bytes: 0,
        in_line: false,
        eof_symbol: false,
    };
    let mut body = vec![blank.clone(); text_width * body_height];
    let mut ui = vec![blank; width * height];
    let mut eof_row = None;
    let mut last_source = None;
    let mut source_position = 0;
    let mut visible_start = None;
    walk_cells(data, wrap_width, tab_width, |cell| {
        let file_bytes = if last_source == Some(cell.pos) {
            0
        } else {
            cell.len
        };
        last_source = Some(cell.pos);
        if cell.row >= top {
            visible_start.get_or_insert(source_position);
        }
        source_position += file_bytes;
        if matches!(cell.kind, CellKind::Eof) {
            eof_row = Some(cell.row);
        }
        if cell.row >= top + body_height {
            return false;
        }
        if cell.row < top {
            return true;
        }
        let row = cell.row - top;
        if cell.width == 0 {
            let mut column = cell.column.min(width);
            while column > 0 {
                column -= 1;
                if body[row * text_width + column].width > 0 {
                    body[row * width + column]
                        .text
                        .push_str(&display_token(data, cell.pos, cell.column).0);
                    body[row * width + column].file_bytes += file_bytes;
                    break;
                }
            }
            return true;
        }
        let token = match cell.kind {
            CellKind::Source => display_token(data, cell.pos, cell.column).0,
            CellKind::Replacement => "?".into(),
            CellKind::Space | CellKind::Eof => " ".into(),
        };
        let index = row * text_width + cell.column;
        body[index].text = token;
        body[index].width = cell.width;
        body[index].source = Some(cell.pos);
        if data
            .get(cell.pos)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n'))
        {
            body[index].style = base.merge(whitespace);
        }
        body[index].file_bytes = file_bytes;
        body[index].in_line = !matches!(cell.kind, CellKind::Eof);
        for offset in 1..cell.width {
            body[index + offset].text.clear();
            body[index + offset].width = 0;
            body[index + offset].source = Some(cell.pos);
            body[index + offset].style = body[index].style;
            body[index + offset].in_line = true;
        }
        true
    });
    if display.showeof {
        if let Some(eof) = eof_row {
            for row in 0..body_height {
                if top + row > eof {
                    body[row * text_width].text = "~".into();
                    body[row * text_width].eof_symbol = true;
                }
            }
        }
    }
    let mut copied = false;
    for operation in &decorations.operations {
        match operation {
            DrawOperation::Range { style, range } => {
                paint_range(
                    &mut body,
                    text_width,
                    visible_start.unwrap_or(0),
                    range,
                    *style,
                );
            }
            DrawOperation::CoreOverlays {
                selection,
                cursor,
                primary,
                eof,
            } => {
                for cell in &mut body {
                    if cell.source.is_some_and(|position| {
                        selections.iter().any(|range| {
                            range.contains(&position) || range.is_empty() && range.start == position
                        })
                    }) {
                        cell.style = cell.style.merge(*selection);
                    }
                    if cell.eof_symbol {
                        cell.style = base.merge(*eof);
                    }
                }
                let start = body
                    .iter()
                    .filter_map(|cell| cell.source)
                    .min()
                    .unwrap_or(0);
                let end = body
                    .iter()
                    .filter_map(|cell| cell.source)
                    .max()
                    .map_or(start, |position| motion::char_next(data, position));
                for (position, is_primary) in &overlay.cursors {
                    let (row, column) = wrapped_cursor(data, *position, text_width, tab_width);
                    if row >= top && row < top + body_height && column < text_width {
                        let cell = &mut body[(row - top) * text_width + column];
                        cell.style = cell
                            .style
                            .merge(if *is_primary { *primary } else { *cursor });
                    }
                    if overlay.match_brackets {
                        if let Some(matching) =
                            motion::bracket_match_range(data, *position, start..end)
                                .filter(|matching| matching != position)
                        {
                            let (row, column) =
                                wrapped_cursor(data, matching, text_width, tab_width);
                            if row >= top && row < top + body_height && column < text_width {
                                let cell = &mut body[(row - top) * text_width + column];
                                cell.style = cell.style.merge(*selection);
                            }
                        }
                    }
                }
            }
            DrawOperation::Cell { x, y, style } if *x < width && *y < height => {
                let cell = &mut ui[y * width + x];
                cell.style = cell.style.merge(*style);
            }
            DrawOperation::Status { left, right, style } => {
                let left_width = source_column(left, 0, left.len());
                let right_width = source_column(right, 0, right.len());
                let spaces = width
                    .saturating_sub(left_width.saturating_add(right_width))
                    .max(1);
                // Native window_status formats into a 1024-byte local buffer.
                let mut label: Vec<_> = left.iter().copied().take(1022).collect();
                label.extend(std::iter::repeat_n(b' ', spaces.min(1022 - label.len())));
                label.extend(right.iter().copied().take(1022 - label.len()));
                paint_text(
                    &mut ui[(height - 1) * width..height * width],
                    0,
                    &label,
                    *style,
                );
            }
            DrawOperation::BodyCopy => {
                copy_body(&mut ui, &body, width, text_width, sidebar, body_height);
                copied = true;
            }
            DrawOperation::Cell { .. } => {}
        }
    }
    if !copied {
        copy_body(&mut ui, &body, width, text_width, sidebar, body_height);
    }
    if sidebar > 0 {
        draw_sidebar(&mut ui, numbers, width, body_height, base, decorations);
    }
    let mut output = String::new();
    for (row, cells) in ui.chunks(width).enumerate() {
        output.push_str(&format!("\x1b[{};{}H", region.y + row + 1, region.x + 1));
        let mut previous = None;
        for cell in cells {
            if cell.width == 0 {
                continue;
            }
            if previous != Some(cell.style) {
                output.push_str(&cell.style.to_sgr());
                previous = Some(cell.style);
            }
            output.push_str(&cell.text);
        }
        output.push_str("\x1b[0m");
    }
    output
}

/// Native line number sidebar: the width it occupies and the number of each
/// visible row.
struct LineNumbers {
    width: usize,
    numbers: Vec<usize>,
    /// Line number of the cursor, which uses the cursor line number style.
    cursor_line: usize,
}

impl LineNumbers {
    fn none() -> Self {
        Self {
            width: 0,
            numbers: Vec::new(),
            cursor_line: 1,
        }
    }

    /// The number drawn in a visible row.
    fn number(&self, row: usize) -> Option<usize> {
        self.numbers.get(row).copied()
    }
}

/// The width of the native line number sidebar. The native sidebar is as wide
/// as the last visible line number plus a separating space, and never
/// narrower than the `numberwidth` option.
fn sidebar_columns(
    data: &[u8],
    top: usize,
    body: usize,
    columns: usize,
    tab_width: usize,
    display: &WindowDisplay,
) -> usize {
    if !display.numbers && !display.relativenumbers {
        return 0;
    }
    let last = if body == 0 {
        1
    } else {
        crate::view::line_number_at_row(data, top + body - 1, columns, tab_width).unwrap_or(1)
    };
    (last.to_string().len() + 1)
        .max(display.numberwidth)
        .min(columns)
}

/// The number of every visible row, honouring the native relative numbering.
#[allow(clippy::too_many_arguments)]
fn line_numbers(
    data: &[u8],
    top: usize,
    body: usize,
    columns: usize,
    tab_width: usize,
    display: &WindowDisplay,
    cursor_line: usize,
    width: usize,
) -> LineNumbers {
    if width == 0 {
        return LineNumbers::none();
    }
    let lines = crate::view::row_line_numbers(data, top..top + body, columns, tab_width);
    let numbers = lines
        .into_iter()
        .map(|line| {
            if display.relativenumbers && !display.numbers {
                line.abs_diff(cursor_line)
            } else {
                line
            }
        })
        .collect();
    LineNumbers {
        width,
        numbers,
        cursor_line,
    }
}

/// Copy the body rows into the window grid, leaving the sidebar columns.
fn copy_body(
    ui: &mut [PaintCell],
    body: &[PaintCell],
    width: usize,
    text_width: usize,
    sidebar: usize,
    body_height: usize,
) {
    for row in 0..body_height {
        let start = row * width + sidebar;
        let source = row * text_width;
        let length = text_width.min(body.len().saturating_sub(source));
        ui[start..start + length].clone_from_slice(&body[source..source + length]);
    }
}

/// Draw the native line number sidebar: the number of every visible row,
/// right aligned in front of the text.
fn draw_sidebar(
    ui: &mut [PaintCell],
    numbers: &LineNumbers,
    width: usize,
    body_height: usize,
    base: Style,
    decorations: &ViewDecorations,
) {
    let sidebar = numbers.width;
    for row in 0..body_height {
        let Some(number) = numbers.number(row) else {
            continue;
        };
        let text = number.to_string();
        let digits = text.len();
        let padding = sidebar.saturating_sub(1 + digits);
        let style = base.merge(resolved_style(
            Some(decorations),
            if numbers.cursor_line == number { 6 } else { 5 },
        ));
        for column in 0..sidebar {
            let cell = &mut ui[row * width + column];
            cell.style = style;
            cell.width = 1;
            cell.text.clear();
        }
        for (offset, character) in text.chars().enumerate() {
            let cell = &mut ui[row * width + padding + offset];
            cell.text = character.to_string();
            cell.width = 1;
            cell.style = style;
        }
    }
}

fn resolved_style(decorations: Option<&ViewDecorations>, id: usize) -> Style {
    decorations
        .and_then(|d| d.styles.get(&id))
        .copied()
        .unwrap_or_else(|| {
            let attributes = match id {
                1 | 4 | 7 | 8 => "reverse",
                2 => "reverse,blink",
                9 => "reverse,bold",
                11 => "bold",
                _ => "",
            };
            Style::terminal_default().merge(Style::parse(attributes))
        })
}

fn append_styled_padding(
    output: &mut String,
    columns: Range<usize>,
    row: usize,
    base: Style,
    decorations: &ViewDecorations,
) {
    let mut previous = None;
    for column in columns {
        let style = decorations.cells.get(&(column, row)).map_or(base, |id| {
            base.merge(resolved_style(Some(decorations), *id))
        });
        if previous != Some(style) {
            output.push_str(&style.to_sgr());
            previous = Some(style);
        }
        output.push(' ');
    }
}

fn styled_label(
    label: &[u8],
    columns: usize,
    row: usize,
    base: Style,
    decorations: &ViewDecorations,
) -> String {
    let mut output = String::new();
    let mut position = 0;
    let mut column = 0;
    let mut previous = None;
    while position < label.len() {
        let (token, width, length) = display_token(label, position, column);
        if column + width > columns {
            break;
        }
        let style = decorations.cells.get(&(column, row)).map_or(base, |id| {
            base.merge(resolved_style(Some(decorations), *id))
        });
        if previous != Some(style) {
            output.push_str(&style.to_sgr());
            previous = Some(style);
        }
        output.push_str(&token);
        column += width;
        position += length;
    }
    append_styled_padding(&mut output, column..columns, row, base, decorations);
    output.push_str("\x1b[0m");
    output
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // These are selection ranges, not integer collections.
mod tests {
    use super::*;

    #[test]
    fn cancelling_a_partial_paste_keeps_discarding_its_remaining_command_bytes() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[200~partial");
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.cancel();
        decoder.push(b":q!\r\x1b[201~j");
        assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Timeout));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('j')))
        );
    }

    #[test]
    fn decoder_handles_fragmented_keys_and_utf8() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[");
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.push(b"A\xe7");
        assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Key(Key::Up)));
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.push(b"\x95\x8c\x12\x7f\t\r");
        for key in [
            Key::Char('界'),
            Key::Ctrl('r'),
            Key::Backspace,
            Key::Tab,
            Key::Enter,
        ] {
            assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Key(key)));
        }
        decoder.push(b"\x1b");
        assert_eq!(decoder.next(false).unwrap(), None);
        assert_eq!(
            decoder.next(true).unwrap(),
            Some(InputEvent::Key(Key::Escape))
        );
    }

    #[test]
    fn decoder_preserves_meta_control_backtab_and_named_keys() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b\x0b\x1b\x0a\x1b[Z\x1b[5~\x1b[6~\x1b[2~\x1b[1;2D\x1b[1;2C\x1b[5;2~\x1b[6;2~\x1bA\x1b\xc3");
        for key in [
            Key::AltCtrl('k'),
            Key::AltCtrl('j'),
            Key::BackTab,
            Key::PageUp,
            Key::PageDown,
            Key::Insert,
            Key::ShiftLeft,
            Key::ShiftRight,
            Key::ShiftPageUp,
            Key::ShiftPageDown,
            Key::Alt('A'),
        ] {
            assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Key(key)));
        }
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.push(b"\xa9");
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Alt('é')))
        );
        decoder.push(b"\x1b[1;5Ax");
        assert!(matches!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Message(_))
        ));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('x')))
        );
        decoder.push(b"\x0a\x0d");
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Ctrl('j')))
        );
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Enter))
        );
        decoder.push(b"\x1bOP\x1b[24~\x1b[34~");
        for number in [1, 12, 20] {
            assert_eq!(
                decoder.next(false).unwrap(),
                Some(InputEvent::Key(Key::Function(number)))
            );
        }
    }

    #[test]
    fn paste_never_becomes_commands_and_preserves_arbitrary_bytes() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[200~:q!\n\0\xff\x1b[20");
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.push(b"1~j");
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Paste(b":q!\n\0\xff".to_vec()))
        );
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('j')))
        );
    }

    #[test]
    fn oversized_paste_is_reported_once_and_discarded_until_its_end() {
        let mut decoder = Decoder {
            paste: Some(vec![b'x'; MAX_PASTE]),
            ..Decoder::default()
        };
        decoder.push(b":q! extra data");
        assert!(matches!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Message(_))
        ));
        assert!(decoder.paste.as_ref().unwrap().is_empty());
        decoder.push(b"more :q!\r\x1b[201~j");
        assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Timeout));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('j')))
        );
        assert!(decoder.paste.is_none());
    }

    #[test]
    fn unsupported_escape_sequences_are_consumed_as_a_unit() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[99~x");
        assert!(matches!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Message(_))
        ));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('x')))
        );
        decoder.push(b"\x1b]incomplete :q!");
        assert_eq!(decoder.next(false).unwrap(), None);
        assert!(decoder.pending.is_empty());
        decoder.push(b"title\x1b");
        assert_eq!(decoder.next(false).unwrap(), None);
        decoder.push(b"\\j");
        assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Timeout));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('j')))
        );
    }

    #[test]
    fn oversized_unfinished_csi_does_not_accumulate_or_dispatch_payload() {
        let mut decoder = Decoder::default();
        decoder.push(b"\x1b[");
        decoder.push(&[b'1'; 4096]);
        assert_eq!(decoder.next(false).unwrap(), None);
        assert!(decoder.pending.is_empty());
        decoder.push(&[b'2'; 4096]);
        assert_eq!(decoder.next(false).unwrap(), None);
        assert!(decoder.pending.is_empty());
        decoder.push(b"~x");
        assert_eq!(decoder.next(false).unwrap(), Some(InputEvent::Timeout));
        assert_eq!(
            decoder.next(false).unwrap(),
            Some(InputEvent::Key(Key::Char('x')))
        );
    }

    #[test]
    fn content_and_status_cannot_inject_terminal_controls() {
        let data = b"\x1b[2J\0\x7f\xff\xc2\x9b";
        let line = clipped_line(data, 0..data.len(), 0, 80, &[]);
        assert_eq!(line, "^[[2J^@^?<FF>\\u{9b}");
        assert!(!line.as_bytes().contains(&0x1b));
        assert_eq!(clipped_line(b"a\tb", 0..3, 0, 12, &[]), "a       b");
    }

    #[test]
    fn distinct_selections_are_highlighted_without_selecting_the_gap() {
        assert_eq!(
            clipped_line(b"abcdef", 0..6, 0, 80, &[0..1, 4..6]),
            "\x1b[7ma\x1b[0mbcd\x1b[7mef\x1b[0m"
        );
    }

    #[test]
    fn empty_eof_and_newline_cursors_have_visible_cells() {
        let line = render_line(b"abc", 0..3, 0, 8, &[3..3]);
        assert_eq!(line.text, "abc\x1b[7m \x1b[0m");
        assert_eq!(line.width, 4);
        let empty = render_line(b"", 0..0, 0, 8, &[0..0]);
        assert_eq!(empty.text, "\x1b[7m \x1b[0m");
        assert_eq!(empty.width, 1);
        let newline = render_line(b"a\n", 0..1, 0, 8, &[1..2]);
        assert_eq!(newline.text, "a\x1b[7m \x1b[0m");
    }

    fn view<'a>(
        id: usize,
        data: &'a [u8],
        cursor: usize,
        selections: &'a [crate::selection::Selection],
    ) -> RenderView<'a> {
        RenderView {
            id,
            data,
            cursor,
            selections,
            anchored: &[],
            selections_visible: true,
            primary_index: 0,
            mode: Mode::Normal,
            path: Path::new("file"),
            modified: false,
            tab_width: 8,
            display: WindowDisplay::default(),
            prompt_executing: false,
            topline: None,
            wrapcolumn: 0,
            anchor: None,
            prompt: None,
        }
    }

    #[test]
    fn measured_geometry_tracks_wrapped_source_and_excludes_status() {
        let views = [view(1, b"abcdef\nghij", 11, &[])];
        let mut viewports = BTreeMap::new();
        compose_views(&mut viewports, &views, 1, Layout::Horizontal, "", (3, 3)).unwrap();
        let geometry = viewports[&1].geometry.as_ref().unwrap();
        assert_eq!((geometry.width, geometry.height), (3, 2));
        assert_eq!(geometry.bytes, 7..11);
        assert_eq!(geometry.lines, 2..3);
        assert_eq!(screen_position(views[0].data, 9, 3, 8), (3, 2));
    }

    #[test]
    fn oneline_prompt_reserves_full_width_below_vertical_splits() {
        let mut prompt = view(3, b":old\n:new\n", 8, &[]);
        prompt.display = WindowDisplay {
            oneline: true,
            statusbar: false,
            showeof: false,
            ..WindowDisplay::default()
        };
        let views = [view(1, b"left", 0, &[]), view(2, b"right", 0, &[]), prompt];
        let mut ports = BTreeMap::new();
        let frame = compose_views(&mut ports, &views, 3, Layout::Vertical, "", (12, 6)).unwrap();
        let prompt = ports[&3].geometry.as_ref().unwrap();
        assert_eq!(
            (
                prompt.x,
                prompt.y,
                prompt.width,
                prompt.height,
                prompt.total_height
            ),
            (0, 5, 12, 1, 1)
        );
        assert_eq!(ports[&1].geometry.as_ref().unwrap().height, 4);
        assert!(frame.contains("\x1b[6;1H:new"));
        assert!(frame.ends_with("\x1b[6;4H\x1b[?25h"));
        assert!(!frame.contains("\x1b[6;6H|"));
    }

    #[test]
    fn expanded_prompt_has_real_text_cursor_no_status_and_optional_eof() {
        let mut prompt = view(2, b":old\n:new\n", 5, &[]);
        prompt.display = WindowDisplay {
            oneline: false,
            statusbar: false,
            showeof: false,
            ..WindowDisplay::default()
        };
        let mut ports = BTreeMap::new();
        let frame = compose_views(
            &mut ports,
            &[view(1, b"file", 0, &[]), prompt],
            2,
            Layout::Horizontal,
            "",
            (20, 8),
        )
        .unwrap();
        let geometry = ports[&2].geometry.as_ref().unwrap();
        assert_eq!(
            (geometry.y, geometry.height, geometry.total_height),
            (4, 4, 4)
        );
        assert!(frame.contains("\x1b[5;1H:old"));
        assert!(frame.contains("\x1b[6;1H:new"));
        assert!(frame.ends_with("\x1b[6;1H\x1b[?25h"));
        assert!(!frame.contains("\x1b[8;1H~"));
    }

    #[test]
    fn statusless_errors_reserve_an_escaped_global_info_row_even_when_tiny() {
        let mut prompt = view(2, b":q", 2, &[]);
        prompt.display = WindowDisplay {
            oneline: true,
            statusbar: false,
            showeof: false,
            ..WindowDisplay::default()
        };
        let mut ports = BTreeMap::new();
        let frame = compose_views(
            &mut ports,
            &[prompt],
            2,
            Layout::Horizontal,
            "bad\x1b[2J",
            (20, 3),
        )
        .unwrap();
        assert_eq!(ports[&2].geometry.as_ref().unwrap().y, 1);
        assert!(frame.contains("\x1b[3;1H\x1b[0mbad^[[2J"));
        assert!(frame.ends_with("\x1b[2;3H\x1b[?25h"));
        let mut prompt = view(2, b":q", 2, &[]);
        prompt.display.statusbar = false;
        let frame = compose_views(
            &mut ports,
            &[prompt],
            2,
            Layout::Horizontal,
            "error",
            (1, 1),
        )
        .unwrap();
        assert!(frame.ends_with("\x1b[1;1H\x1b[?25h"));
        assert!(ports[&2].geometry.is_none());
    }

    #[test]
    fn plugin_styles_render_ranges_cells_and_safe_status() {
        let views = [view(1, b"abc", 0, &[])];
        let style = ViewDecorations {
            styles: BTreeMap::from([
                (20, Style::parse("fore:red,bold")),
                (21, Style::parse("back:#010203")),
            ]),
            ranges: vec![(20, 1..2)],
            cells: BTreeMap::from([((2, 0), 21)]),
            status_left: b"Left".to_vec(),
            status_right: b"R".to_vec(),
            ..ViewDecorations::default()
        };
        let mut viewports = BTreeMap::new();
        let frame = compose_views_decorated(
            &mut viewports,
            &views,
            1,
            Layout::Horizontal,
            "",
            (8, 3),
            &BTreeMap::from([(1, style)]),
        )
        .unwrap();
        assert!(frame.contains("\x1b[1;31;40mb"));
        assert!(frame.contains("\x1b[37;48;2;1;2;3mc"));
        assert!(frame.contains("Left   R"));
        let message_frame = compose_views_decorated(
            &mut viewports,
            &views,
            1,
            Layout::Horizontal,
            "error",
            (8, 4),
            &BTreeMap::from([(
                1,
                ViewDecorations {
                    status_left: b"Left".to_vec(),
                    ..ViewDecorations::default()
                },
            )]),
        )
        .unwrap();
        assert!(message_frame.contains("Left"));
        assert!(message_frame.contains("\x1b[4;1H\x1b[0m\x1b[0m\x1b[1;37;40merror"));
        assert_eq!(viewports[&1].geometry.as_ref().unwrap().total_height, 3);
        assert_eq!(
            Style::parse("fore:red\x1b[2J,bold").to_sgr(),
            "\x1b[0m\x1b[1m"
        );
    }

    #[test]
    fn range_highlights_follow_anchors_independently_of_global_mode() {
        let selections = [crate::selection::Selection {
            anchor: 0,
            cursor: 2,
        }];
        let mut anchored = view(1, b"abcdef", 2, &selections);
        anchored.anchored = &[true];
        assert!(highlights(&anchored, true).contains(&(0..3)));
        anchored.mode = Mode::VisualLine;
        assert!(highlights(&anchored, false).contains(&(0..3)));
        assert!(!highlights(&anchored, false).contains(&(0..6)));
        let mut unanchored = view(1, b"abcdef", 2, &selections);
        unanchored.mode = Mode::Visual;
        assert!(highlights(&unanchored, true).is_empty());
        assert!(highlights(&unanchored, false).is_empty());
        anchored.selections_visible = false;
        assert!(highlights(&anchored, false).is_empty());
    }

    #[test]
    fn typed_merges_clear_attributes_and_style_overlapping_ranges_eof_and_blanks() {
        let styles = ViewDecorations {
            styles: BTreeMap::from([
                (0, Style::parse("bold,fore:blue,back:white")),
                (20, Style::parse("italics,fore:green")),
                (21, Style::parse("keep_attribute,underlined,back:red")),
                (22, Style::parse("dim,back:200")),
            ]),
            ranges: vec![(20, 0..3), (21, 1..3)],
            cells: BTreeMap::from([((3, 0), 22)]),
            ..ViewDecorations::default()
        };
        let lines = wrapped_rows_decorated(b"ab", 5, 8, 0..1, &[], Some(&styles), true);
        assert!(
            lines[0].text.contains("\x1b[3;32;47ma"),
            "{:?}",
            lines[0].text
        );
        assert!(
            lines[0].text.contains("\x1b[3;4;32;41mb "),
            "{:?}",
            lines[0].text
        );
        assert!(
            lines[0].text.contains("\x1b[2;34;48;5;200m "),
            "{:?}",
            lines[0].text
        );
        assert_eq!(lines[0].width, 5);
    }

    #[test]
    fn cell_styles_apply_to_status_padding_and_can_clear_status_attributes() {
        let views = [view(1, b"a", 0, &[])];
        let styles = ViewDecorations {
            styles: BTreeMap::from([
                (20, Style::parse("fore:yellow")),
                (21, Style::parse("back:red")),
            ]),
            cells: BTreeMap::from([((0, 1), 20), ((5, 1), 21)]),
            status_left: b"L".to_vec(),
            ..ViewDecorations::default()
        };
        let frame = compose_views_decorated(
            &mut BTreeMap::new(),
            &views,
            1,
            Layout::Horizontal,
            "",
            (6, 2),
            &BTreeMap::from([(1, styles)]),
        )
        .unwrap();
        assert!(frame.contains("\x1b[33;40mL"), "{frame:?}");
        assert!(frame.contains("\x1b[37;41m "), "{frame:?}");
    }

    #[test]
    fn ordered_paint_respects_body_copy_core_phase_and_repeated_status_cell_merges() {
        let base = Style::parse("bold,fore:blue,back:white");
        let mut decoration = ViewDecorations {
            base: Some(base),
            operations: vec![
                DrawOperation::Range {
                    style: Style::parse("italics,fore:green"),
                    range: 0..1,
                },
                DrawOperation::CoreOverlays {
                    selection: Style::parse("reverse"),
                    cursor: Style::default(),
                    primary: Style::default(),
                    eof: Style::default(),
                },
                DrawOperation::Range {
                    style: Style::parse("underlined,back:red"),
                    range: 0..1,
                },
                DrawOperation::Cell {
                    x: 0,
                    y: 0,
                    style: Style::parse("fore:yellow"),
                },
                DrawOperation::Cell {
                    x: 0,
                    y: 1,
                    style: Style::parse("fore:magenta"),
                },
                DrawOperation::Status {
                    left: b"L".to_vec(),
                    right: b"R".to_vec(),
                    style: base.merge(Style::parse("reverse")),
                },
                DrawOperation::Cell {
                    x: 0,
                    y: 1,
                    style: Style::parse("fore:yellow"),
                },
                DrawOperation::Cell {
                    x: 1,
                    y: 1,
                    style: Style::parse("italics,fore:green"),
                },
                DrawOperation::Cell {
                    x: 1,
                    y: 1,
                    style: Style::parse("keep_attribute,underlined,back:red"),
                },
                DrawOperation::BodyCopy,
                DrawOperation::Cell {
                    x: 0,
                    y: 0,
                    style: Style::parse("keep_attribute,dim"),
                },
                DrawOperation::Range {
                    style: Style::parse("fore:cyan"),
                    range: 0..1,
                },
            ],
            ..ViewDecorations::default()
        };
        let region = Region {
            x: 0,
            y: 0,
            width: 4,
            height: 2,
        };
        let frame = ordered_panel(
            b"ab",
            0,
            &[0..1],
            region,
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            region.width,
        );
        assert!(frame.contains("\x1b[2;4;32;41ma"), "{frame:?}");
        assert!(frame.contains("\x1b[33;47mL"), "{frame:?}");
        assert!(frame.contains("\x1b[3;4;32;41m "), "{frame:?}");
        assert!(!frame.contains("\x1b[36;41ma"), "{frame:?}");
        decoration.operations.push(DrawOperation::Status {
            left: b"Z".to_vec(),
            right: Vec::new(),
            style: base.merge(Style::parse("fore:cyan")),
        });
        let frame = ordered_panel(
            b"ab",
            0,
            &[0..1],
            region,
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            region.width,
        );
        assert!(
            frame.contains("\x1b[2;1H\x1b[0m\x1b[36;47mZ   "),
            "{frame:?}"
        );
    }

    #[test]
    fn native_range_endpoints_respect_zero_byte_tab_control_and_eof_cells() {
        let mut decoration = ViewDecorations {
            operations: vec![
                DrawOperation::Range {
                    style: Style::parse("bold,fore:red"),
                    range: 1..2,
                },
                DrawOperation::BodyCopy,
            ],
            ..ViewDecorations::default()
        };
        let region = Region {
            x: 0,
            y: 0,
            width: 12,
            height: 2,
        };
        let tab = ordered_panel(
            b"A\tZ",
            0,
            &[],
            region,
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            region.width,
        );
        assert!(
            tab.contains("\x1b[1;31;40m \x1b[0m\x1b[37;40m      Z"),
            "{tab:?}"
        );
        let control = ordered_panel(
            b"A\x01Z",
            0,
            &[],
            region,
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            region.width,
        );
        assert!(
            control.contains("\x1b[1;31;40m^A\x1b[0m\x1b[37;40mZ"),
            "{control:?}"
        );
        decoration.operations[0] = DrawOperation::Range {
            style: Style::parse("fore:red"),
            range: 1..2,
        };
        let eof = ordered_panel(
            b"A",
            0,
            &[],
            region,
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            region.width,
        );
        assert!(!eof.contains("31;40m"), "{eof:?}");
    }

    #[test]
    fn whitespace_style_is_a_pre_highlight_snapshot_and_does_not_recolor_eof_padding() {
        let decoration = ViewDecorations {
            base: Some(Style::parse("bold,fore:blue,back:white")),
            whitespace: Some(Style::parse("italics,fore:red")),
            styles: BTreeMap::from([(13, Style::parse("underlined,fore:green"))]),
            operations: vec![DrawOperation::BodyCopy],
            ..ViewDecorations::default()
        };
        let frame = ordered_panel(
            b"A \tZ\n",
            0,
            &[],
            Region {
                x: 0,
                y: 0,
                width: 12,
                height: 3,
            },
            8,
            WindowDisplay::default(),
            &decoration,
            &OverlayContext::default(),
            &LineNumbers::none(),
            12,
        );
        assert!(
            frame.contains("\x1b[3;31;47m       \x1b[0m\x1b[1;34;47mZ"),
            "{frame:?}"
        );
        assert!(frame.contains("\x1b[2;1H\x1b[0m\x1b[1;34;47m"), "{frame:?}");
        assert!(!frame.contains("32;47m"), "{frame:?}");
    }

    #[test]
    fn split_panels_do_not_erase_neighbors_and_focus_hardware_cursor() {
        let selections = [crate::selection::Selection {
            anchor: 0,
            cursor: 0,
        }];
        let views = [
            view(11, b"Left", 0, &selections),
            view(22, b"Right", 0, &selections),
        ];
        let mut viewports = BTreeMap::new();
        let vertical =
            compose_views(&mut viewports, &views, 11, Layout::Vertical, "", (12, 4)).unwrap();
        assert!(vertical.contains("\x1b[1;1H\x1b[0m\x1b[5;7;37;40mL"));
        assert!(vertical.contains("\x1b[1;7H\x1b[0m\x1b[37;40mRight"));
        assert!(vertical.contains("\x1b[4;6H|"));
        assert!(!vertical.contains("\x1b[K"));
        assert!(vertical.ends_with("\x1b[1;1H\x1b[?25h"));
        let horizontal =
            compose_views(&mut viewports, &views, 22, Layout::Horizontal, "", (12, 5)).unwrap();
        assert!(horizontal.contains("\x1b[3;1H\x1b[0m\x1b[5;7;37;40mR"));
        assert!(horizontal.ends_with("\x1b[3;1H\x1b[?25h"));
    }

    #[test]
    fn viewport_ids_survive_reorder_and_tiny_terminal_keeps_active_view() {
        let selections = [crate::selection::Selection {
            anchor: 0,
            cursor: 0,
        }];
        let data = b"a\nb\nc\nd\ne\nf\n";
        let mut first = view(1, data, data.len(), &selections);
        first.selections = &[];
        let second = view(2, b"second", 0, &selections);
        let mut viewports = BTreeMap::new();
        compose_views(
            &mut viewports,
            &[first, second],
            1,
            Layout::Vertical,
            "",
            (30, 4),
        )
        .unwrap();
        let saved_top = viewports[&1].top;
        assert!(saved_top > 0);
        compose_views(
            &mut viewports,
            &[
                view(2, b"second", 0, &selections),
                view(1, data, data.len(), &[]),
            ],
            2,
            Layout::Vertical,
            "",
            (30, 4),
        )
        .unwrap();
        assert_eq!(viewports[&1].top, saved_top);
        let tiny = compose_views(
            &mut viewports,
            &[
                view(1, data, 0, &[]),
                view(2, b"second", 0, &[]),
                view(3, b"third", 0, &[]),
            ],
            2,
            Layout::Horizontal,
            "",
            (1, 1),
        )
        .unwrap();
        assert!(tiny.ends_with("\x1b[1;1H\x1b[?25h"));
        compose_views(
            &mut viewports,
            &[view(2, b"second", 0, &[])],
            2,
            Layout::Horizontal,
            "",
            (10, 3),
        )
        .unwrap();
        assert_eq!(viewports.len(), 1);
        assert!(viewports.contains_key(&2));
        assert!(compose_views(&mut viewports, &[], 2, Layout::Horizontal, "", (1, 1)).is_err());
    }

    #[test]
    fn the_wrap_column_option_wraps_the_text_earlier() {
        let data = b"abcdefghijklmnopqrstuvwxyz\n";
        let region = Region {
            x: 0,
            y: 0,
            width: 40,
            height: 4,
        };
        let mut viewport = Viewport::default();
        let display = WindowDisplay {
            wrapcolumn: 10,
            ..WindowDisplay::default()
        };
        let panel = viewport.panel_decorated(
            data,
            0,
            &[],
            "s",
            None,
            region,
            true,
            8,
            None,
            display,
            &OverlayContext::default(),
            None,
            None,
        );
        // Native `view_max_text_width` wraps the text at the column.
        assert!(panel.text.contains("abcdefghij"), "{:?}", panel.text);
        assert!(panel.text.contains("klmnopqrst"), "{:?}", panel.text);
    }

    #[test]
    fn the_native_overlay_path_draws_the_body() {
        let data = b"one\ntwo\nthree\n";
        let region = Region {
            x: 0,
            y: 0,
            width: 12,
            height: 4,
        };
        let mut viewport = Viewport::default();
        let overlay = OverlayContext {
            cursors: Vec::new(),
            match_brackets: false,
            native_view: true,
        };
        let panel = viewport.panel_decorated(
            data,
            0,
            &[],
            "s",
            None,
            region,
            true,
            8,
            None,
            WindowDisplay::default(),
            &overlay,
            None,
            None,
        );
        assert!(panel.text.contains("one"), "{:?}", panel.text);
        assert!(panel.text.contains("three"), "{:?}", panel.text);
    }

    #[test]
    fn the_line_number_sidebar_labels_the_visible_rows() {
        let data = b"one\ntwo\nthree\n";
        let region = Region {
            x: 0,
            y: 0,
            width: 12,
            height: 4,
        };
        let mut viewport = Viewport::default();
        let display = WindowDisplay {
            numbers: true,
            ..WindowDisplay::default()
        };
        let panel = viewport.panel_decorated(
            data,
            0,
            &[],
            "s",
            None,
            region,
            true,
            8,
            None,
            display,
            &OverlayContext::default(),
            None,
            None,
        );
        // The sidebar holds the line number and a separating space.
        assert!(panel.text.contains("1 one"), "{:?}", panel.text);
        assert!(panel.text.contains("2 two"), "{:?}", panel.text);
        // The body keeps the remaining columns.
        assert!(panel.text.contains("three"), "{:?}", panel.text);
    }

    #[test]
    fn requested_topline_starts_the_frame_there_and_still_follows_the_cursor() {
        let mut viewport = Viewport::default();
        let data = b"a\nb\nc\nd\n";
        let region = Region {
            x: 0,
            y: 0,
            width: 5,
            height: 3,
        };
        let panel = viewport.panel_decorated(
            data,
            0,
            &[],
            "s",
            None,
            region,
            true,
            8,
            None,
            WindowDisplay::default(),
            &OverlayContext::default(),
            Some(2),
            None,
        );
        assert!(panel.text.contains("\x1b[1;1H"));
        assert!(!panel.text.contains("a "), "row zero is scrolled away");
        assert!(panel.text.contains("c"));
        assert_eq!(viewport.top, 2);
        // A request past the text clamps to the final row instead of showing
        // an entirely empty body.
        let panel = viewport.panel_decorated(
            data,
            0,
            &[],
            "s",
            None,
            region,
            true,
            8,
            None,
            WindowDisplay::default(),
            &OverlayContext::default(),
            Some(9),
            None,
        );
        assert!(!panel.text.contains('a'));
        assert_eq!(viewport.top, 4);
    }

    #[test]
    fn viewport_scrolls_softwrapped_rows_and_keeps_prompt_cursor_in_bounds() {
        let mut viewport = Viewport::default();
        let data = b"one\ntwo\nthree\n0123456789";
        let frame = viewport.frame(data, data.len(), &[], "status", None, (5, 3));
        // C view_add_cell wraps a full row before its newline cell, so the
        // five-byte "three" line uses two screen rows at this width.
        assert_eq!(viewport.top, 5);
        assert!(frame.contains("\x1b[1;1H56789"));
        assert!(frame.ends_with("\x1b[2;1H\x1b[?25h"));
        let frame = viewport.frame(data, 0, &[], "status", Some(":long command"), (5, 1));
        assert!(frame.ends_with("\x1b[1;5H\x1b[?25h"));
    }

    #[test]
    fn resized_frame_and_labels_keep_escape_bytes_out_of_user_text() {
        let mut viewport = Viewport::default();
        let data = b"one\ntwo\nthree\n0123456789";
        viewport.frame(data, data.len(), &[], "file", None, (80, 24));
        let frame = viewport.frame(data, data.len(), &[], "bad\x1b[2J\n", None, (12, 2));
        assert_eq!(viewport.top, 3);
        assert!(frame.contains("bad^[[2J^J"));
        assert!(!frame.contains("\x1b[2J"));
        assert!(frame.ends_with("\x1b[1;11H\x1b[?25h"));
        let prompt = viewport.frame(data, 0, &[], "file", Some("/a\x1b[2J"), (20, 2));
        assert!(prompt.contains("/a^[[2J"));
        assert!(!prompt.contains("\x1b[2J"));
    }

    #[test]
    fn default_softwrap_keeps_cells_and_newline_width_like_c_view() {
        let rows = wrapped_rows(b"abcde\nX", 5, 8, 0..4, &[]);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["abcde", " ", "X ", "~"]
        );
        assert_eq!(wrapped_cursor(b"abcde\nX", 5, 5, 8), (1, 0));
        assert_eq!(wrapped_cursor(b"abcde\nX", 6, 5, 8), (2, 0));
        assert_eq!(wrapped_cursor(b"abcdefghij", 10, 5, 8), (2, 0));
        let rows = wrapped_rows(b"a\tb", 5, 8, 0..2, &[]);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["a    ", "   b "]
        );
        assert_eq!(wrapped_cursor(b"a\tb", 2, 5, 8), (1, 3));
        assert_eq!(wrapped_cursor(b"a\tb", 2, 8, 4), (0, 4));
        assert_eq!(wrapped_cursor(b"a\tb", 2, 8, 8), (1, 0));
    }

    #[test]
    fn wide_combining_and_single_column_wrap_are_bounded() {
        let data = "ab界c".as_bytes();
        assert_eq!(wrapped_cursor(data, 2, 3, 8), (1, 0));
        assert_eq!(wrapped_cursor(data, 5, 3, 8), (1, 2));
        assert_eq!(wrapped_cursor(data, 6, 3, 8), (2, 0));
        let data = "a\u{301}bc".as_bytes();
        let rows = wrapped_rows(data, 2, 8, 0..2, &[]);
        assert_eq!(rows[0].text, "a\u{301}b");
        assert_eq!(rows[1].text, "c ");
        let rows = wrapped_rows("界a".as_bytes(), 1, 8, 0..3, &[]);
        assert_eq!(
            rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>(),
            ["?", "a", " "]
        );
    }
}

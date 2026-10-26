//! File and window ownership for the editor host.
//!
//! A buffer has exactly one `Text`. The active buffer's text lives in `Editor`;
//! inactive texts are parked here. Switching moves ownership, never copies the
//! document or its history. Windows into a shared buffer retain independent
//! selections through stable text marks. Registers and mappings stay global.

use crate::editor::{BufferState, Editor, EditorAction, Key, Mode, PromptActionState};
use crate::file::{Document, SaveMethod};
use crate::jumplist::JumpList;
use crate::selection::{Selection, SelectionId, SelectionMarks, SelectionSet};
use crate::text::Text;
use std::collections::{BTreeMap, BTreeSet};

mod plan;
mod prompt;
mod structural;
pub use plan::{HostPlan, HostStep, SavePlan, command_rejected};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const HELP: &str = "vis-rs: Rust port of the vis editor\n\n\
Normal mode: h/j/k/l move; i/a/I/A/o/O insert; Escape cancels.\n\
Operators: d/c/y with a motion or text object; p/P put; u/Ctrl-r undo/redo.\n\
Visual mode: v/V select; :x/pattern/ creates structural selections.\n\
Files: :w [FILE], :w!, :wq, :e [FILE], :e!, :open FILE.\n\
Windows: :split [FILE], :vsplit [FILE], :new, :vnew.\n\
Ctrl-w w/j/l selects the next window; Ctrl-w p/k/h the previous.\n\
Ctrl-w s/v splits; Ctrl-w c closes; Ctrl-w o keeps only this window.\n\
:q closes a window; :qall closes all clean windows; ! permits discarding edits.\n\
:cd [DIR] changes the workspace directory; :pwd displays it.\n\
This port is in progress. See rust/PORTING.md for compatibility gaps.\n";

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Layout {
    #[default]
    Horizontal,
    Vertical,
}

/// Window geometry and decorations are independent of its buffer's purpose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowDisplay {
    pub oneline: bool,
    pub statusbar: bool,
    pub showeof: bool,
    /// Native `numbers` option: label the rows with absolute line numbers.
    pub numbers: bool,
    /// Native `relativenumbers` option: label the rows with the distance to
    /// the cursor line.
    pub relativenumbers: bool,
    /// Native `numberwidth` option: the minimum sidebar width.
    pub numberwidth: usize,
    /// Native `cursorline` option: highlight the row of the cursor.
    pub cursorline: bool,
    /// Native `colorcolumn` option: highlight a fixed column.
    pub colorcolumn: usize,
    /// Native `showspaces` option: show a replacement symbol for spaces.
    pub showspaces: bool,
    /// Native `showtabs` option: show a replacement symbol for tabs.
    pub showtabs: bool,
    /// Native `shownewlines` option: show a replacement symbol for newlines.
    pub shownewlines: bool,
    /// Native `wrapcolumn` option: wrap the text at this column.
    pub wrapcolumn: usize,
    /// Native `breakat` option: the characters that may end a line.
    pub breakat: std::sync::Arc<str>,
}

impl Default for WindowDisplay {
    fn default() -> Self {
        Self {
            oneline: false,
            statusbar: true,
            showeof: true,
            numbers: false,
            relativenumbers: false,
            numberwidth: 0,
            cursorline: false,
            colorcolumn: 0,
            showspaces: false,
            showtabs: false,
            shownewlines: false,
            wrapcolumn: 0,
            breakat: std::sync::Arc::from(""),
        }
    }
}

struct Buffer {
    text: Text,
    state: BufferState,
    document: Option<Document>,
    read_only: bool,
    stdout: bool,
    save_method: SaveMethod,
    internal: bool,
}

#[derive(Clone)]
struct StoredSelection {
    mark: Option<SelectionMarks>,
    fallback: Selection,
    id: SelectionId,
    anchored: bool,
}

#[derive(Clone)]
struct ViewState {
    selections: Vec<StoredSelection>,
    primary: usize,
    tab_width: usize,
    expand_tab: bool,
    last_visual: Vec<SelectionMarks>,
    /// Per-window jump cache while this window is unfocused. Boxed so the
    /// operation enum's largest variant stays small.
    jumplist: Box<JumpList<Mode>>,
    /// Last measured body geometry for viewport motions.
    viewport: Option<crate::view::ViewportMetrics>,
    /// Native `view->start_mark` of this window: the first displayed row is
    /// tracked by a text mark while the window is unfocused.
    anchor: Option<crate::text::Mark>,
}

impl ViewState {
    fn initial() -> Self {
        Self {
            selections: vec![StoredSelection {
                mark: None,
                id: SelectionId::fresh().expect("selection identity space exhausted"),
                anchored: false,
                fallback: Selection {
                    anchor: 0,
                    cursor: 0,
                },
            }],
            primary: 0,
            tab_width: 8,
            expand_tab: false,
            last_visual: Vec::new(),
            jumplist: Box::new(JumpList::new()),
            viewport: None,
            anchor: None,
        }
    }

    fn capture(editor: &mut Editor) -> Self {
        Self {
            selections: editor
                .view_state()
                .into_iter()
                .map(|(id, selection, anchored)| StoredSelection {
                    mark: editor.selection_marks(id),
                    fallback: selection,
                    id,
                    anchored,
                })
                .collect(),
            primary: editor.primary_index(),
            tab_width: editor.tab_width,
            expand_tab: editor.expand_tab,
            last_visual: editor
                .named_mark_get('^')
                .into_iter()
                .filter_map(|range| {
                    Selection::from_range(
                        &editor.text.to_vec(),
                        range,
                        crate::selection::Direction::Forward,
                    )
                    .ok()?
                    .save(&editor.text)
                    .ok()
                })
                .collect(),
            jumplist: Box::new(editor.take_jumplist()),
            viewport: None,
            anchor: None,
        }
    }

    fn resolve(&self, text: &Text, data: &[u8]) -> Vec<Selection> {
        self.selections
            .iter()
            .map(|saved| {
                let selection = saved
                    .mark
                    .as_ref()
                    .and_then(|mark| mark.restore(text))
                    .unwrap_or(Selection {
                        anchor: saved.fallback.anchor.min(text.len()),
                        cursor: saved.fallback.cursor.min(text.len()),
                    });
                let boundary = |position: usize| {
                    let position = position.min(data.len());
                    if position == data.len() {
                        position
                    } else {
                        crate::motion::char_prev(data, crate::motion::char_next(data, position))
                    }
                };
                Selection {
                    anchor: boundary(selection.anchor),
                    cursor: boundary(selection.cursor),
                }
            })
            .collect()
    }

    fn validate(&self, text: &Text) -> io::Result<()> {
        let data = text.to_vec();
        let primary = self
            .selections
            .get(self.primary)
            .ok_or_else(|| invalid("invalid primary selection"))?
            .id;
        SelectionSet::from_identified(
            &data,
            self.selections
                .iter()
                .map(|s| s.id)
                .zip(self.resolve(text, &data))
                .collect(),
            primary,
        )
        .map_err(|e| invalid(e.to_string()))?;
        Ok(())
    }
}

struct Window {
    id: usize,
    buffer: usize,
    view: ViewState,
    display: WindowDisplay,
    prompt: Option<prompt::PromptSession>,
}

/// Owned render data. Shared buffers are flattened once, then shared by frames.
pub struct Frame {
    pub id: usize,
    pub bytes: Arc<Vec<u8>>,
    pub selections: Vec<Selection>,
    pub anchored: Vec<bool>,
    pub selections_visible: bool,
    pub primary: usize,
    pub mode: Mode,
    pub path: PathBuf,
    pub modified: bool,
    pub display: WindowDisplay,
    pub prompt_executing: bool,
    pub tab_width: usize,
    /// First display row the next frame should show (one-shot scroll request).
    pub topline: Option<usize>,
    /// First display row of the anchored view line, when the text moved it.
    pub anchor: Option<usize>,
    /// Native `wrapcolumn` option: wrap the text at this column.
    pub wrapcolumn: usize,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Outcome {
    Continue,
    Message(String),
    Quit,
    /// Native `vis-suspend` reached the plan; the interactive host restores the
    /// terminal and stops the job. Hosts without a terminal ignore it.
    Suspend,
}

pub struct Workspace {
    pub editor: Editor,
    buffers: Vec<Option<Buffer>>,
    windows: Vec<Window>,
    active: usize,
    next_window: usize,
    directory: PathBuf,
    pub layout: Layout,
    closing: BTreeSet<usize>,
    opening: BTreeSet<usize>,
    message_buffer: Option<usize>,
    message_window: Option<usize>,
    prompt_buffer: Option<usize>,
    next_prompt: u64,
    executing_prompts: BTreeMap<u64, usize>,
    next_structural: u64,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

impl Workspace {
    pub fn new(paths: &[PathBuf], mut input: Option<Vec<u8>>, read_only: bool) -> io::Result<Self> {
        let mut workspace = Self::empty()?;
        if paths.is_empty() {
            workspace.open_scratch(input, read_only)?;
        } else {
            for path in paths {
                let bytes = if path.as_os_str() == "-" {
                    input.take()
                } else {
                    None
                };
                workspace.open_cli_path(path, bytes, read_only)?;
            }
        }
        Ok(workspace)
    }

    pub fn window_count(&self) -> usize {
        self.windows.len()
    }
    pub fn active_window_id(&self) -> usize {
        self.windows[self.active].id
    }
    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn modified(&self) -> bool {
        self.buffers.iter().enumerate().any(|(index, buffer)| {
            buffer.as_ref().is_some_and(|b| !b.internal) && self.text(index).modified()
        })
    }
    pub fn path(&self) -> Option<&Path> {
        self.buffers[self.windows.get(self.active)?.buffer]
            .as_ref()?
            .document
            .as_ref()
            .map(Document::path)
    }

    fn resolve_path(&self, path: &Path) -> PathBuf {
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.directory.join(path)
        };
        // Resolve existing parent components but preserve a final symlink: save
        // policies must still be able to distinguish a link from its target.
        if let (Some(parent), Some(name)) = (path.parent(), path.file_name()) {
            if let Ok(parent) = std::fs::canonicalize(parent) {
                return parent.join(name);
            }
        }
        path
    }

    fn text(&self, buffer: usize) -> &Text {
        if !self.windows.is_empty() && self.windows[self.active].buffer == buffer {
            &self.editor.text
        } else {
            &self.buffers[buffer].as_ref().expect("live buffer").text
        }
    }

    fn same_file(first: &Path, second: &Path) -> bool {
        if first == second {
            return true;
        }
        if let (Ok(first), Ok(second)) =
            (std::fs::canonicalize(first), std::fs::canonicalize(second))
        {
            if first == second {
                return true;
            }
        }
        #[cfg(unix)]
        if let (Ok(first), Ok(second)) = (std::fs::metadata(first), std::fs::metadata(second)) {
            use std::os::unix::fs::MetadataExt;
            return first.dev() == second.dev() && first.ino() == second.ino();
        }
        false
    }

    fn find_buffer(&self, path: &Path) -> Option<usize> {
        self.buffers.iter().position(|buffer| {
            buffer
                .as_ref()
                .and_then(|buffer| buffer.document.as_ref())
                .is_some_and(|document| Self::same_file(document.path(), path))
        })
    }

    fn load(&mut self, path: &Path, read_only: bool) -> io::Result<usize> {
        let path = self.resolve_path(path);
        if let Some(buffer) = self.find_buffer(&path) {
            return Ok(buffer);
        }
        let mut document = Document::open(path)?;
        let text = std::mem::take(&mut document.text);
        let index = self.buffers.len();
        self.buffers.push(Some(Buffer {
            text,
            state: BufferState::default(),
            document: Some(document),
            read_only,
            stdout: false,
            save_method: SaveMethod::Auto,
            internal: false,
        }));
        Ok(index)
    }

    fn add_window(&mut self, buffer: usize, view: ViewState) -> usize {
        let id = self.next_window;
        self.next_window += 1;
        if !self.windows.is_empty() {
            self.active += 1;
        }
        self.windows.insert(
            0,
            Window {
                id,
                buffer,
                view,
                display: WindowDisplay::default(),
                prompt: None,
            },
        );
        id
    }

    fn capture(&mut self) {
        // Geometry belongs to the window, not the focus: keep the last
        // measurement while its editor state moves out.
        let viewport = self.windows[self.active].view.viewport;
        let mut view = ViewState::capture(&mut self.editor);
        view.viewport = viewport;
        view.anchor = self.editor.take_view_anchor();
        self.windows[self.active].view = view;
    }

    fn focus(&mut self, index: usize) -> io::Result<()> {
        self.focus_raw(index)
    }

    fn install_view(&mut self) -> io::Result<()> {
        self.install_view_raw()
    }

    fn install_view_raw(&mut self) -> io::Result<()> {
        let index = self.windows[self.active].buffer;
        self.windows[self.active]
            .view
            .validate(&self.buffers[index].as_ref().expect("live buffer").text)?;
        let target = self.buffers[index].as_mut().expect("live buffer");
        std::mem::swap(&mut self.editor.text, &mut target.text);
        self.editor.swap_buffer_state(&mut target.state);
        self.editor
            .set_window_id(Some(self.windows[self.active].id as u64));
        self.editor.set_internal_scope(target.internal);
        let view = &self.windows[self.active].view;
        self.editor.tab_width = view.tab_width;
        self.editor.expand_tab = view.expand_tab;
        let selections = view
            .selections
            .iter()
            .zip(view.resolve(&self.editor.text, &self.editor.text.to_vec()))
            .map(|(s, p)| (s.id, p, s.anchored))
            .collect();
        self.editor
            .install_view_state_raw(selections, view.selections[view.primary].id)
            .map_err(|e| invalid(e.to_string()))?;
        self.editor
            .restore_selection_marks(
                view.selections
                    .iter()
                    .filter_map(|s| s.mark.clone().map(|m| (s.id, m)))
                    .collect(),
            )
            .map_err(|e| invalid(e.to_string()))?;
        let data = self.editor.text.to_vec();
        let last_visual = view
            .last_visual
            .iter()
            .filter_map(|m| m.restore(&self.editor.text)?.range(&data).ok())
            .collect();
        self.editor
            .named_mark_set('^', last_visual)
            .map_err(|e| invalid(e.to_string()))?;
        let jumplist = std::mem::take(&mut self.windows[self.active].view.jumplist);
        self.editor.install_jumplist(*jumplist);
        let anchor = self.windows[self.active].view.anchor.clone();
        self.editor.install_view_anchor(anchor);
        self.editor
            .set_viewport(self.windows[self.active].view.viewport);
        Ok(())
    }

    fn collect_closed_buffers(&mut self) {
        for (index, buffer) in self.buffers.iter_mut().enumerate() {
            if !buffer.as_ref().is_some_and(|buffer| buffer.internal)
                && !self.opening.contains(&index)
                && !self.windows.iter().any(|window| window.buffer == index)
            {
                if let Some(buffer) = buffer.as_ref() {
                    self.editor.forget_buffer(&buffer.text);
                }
                *buffer = None;
            }
        }
    }

    pub fn frames(&self) -> Vec<Frame> {
        let mut bytes = BTreeMap::new();
        let prompt_parent = self
            .windows
            .get(self.active)
            .and_then(|window| window.prompt.as_ref())
            .and_then(|prompt| prompt.parent);
        self.windows
            .iter()
            .enumerate()
            .map(|(index, window)| {
                let text = self.text(window.buffer);
                let data = Arc::clone(
                    bytes
                        .entry(window.buffer)
                        .or_insert_with(|| Arc::new(text.to_vec())),
                );
                let active = index == self.active;
                let selections = if active {
                    self.editor.selections().to_vec()
                } else {
                    window.view.resolve(text, data.as_slice())
                };
                let anchored = if active {
                    self.editor
                        .selection_ids()
                        .iter()
                        .map(|id| self.editor.selection_anchored(*id).unwrap_or(false))
                        .collect()
                } else {
                    window
                        .view
                        .selections
                        .iter()
                        .map(|selection| selection.anchored)
                        .collect()
                };
                Frame {
                    id: window.id,
                    bytes: data,
                    selections,
                    anchored,
                    selections_visible: active || prompt_parent == Some(window.id),
                    primary: if active {
                        self.editor.primary_index()
                    } else {
                        window.view.primary
                    },
                    mode: self.editor.mode,
                    path: self.buffers[window.buffer]
                        .as_ref()
                        .and_then(|buffer| buffer.document.as_ref())
                        .map_or_else(
                            || PathBuf::from("[No Name]"),
                            |document| document.path().to_path_buf(),
                        ),
                    modified: text.modified(),
                    display: window.display.clone(),
                    prompt_executing: active && !self.executing_prompts.is_empty(),
                    tab_width: if active {
                        self.editor.tab_width
                    } else {
                        window.view.tab_width
                    },
                    topline: if active {
                        self.editor.scroll_request()
                    } else {
                        None
                    },
                    anchor: if active {
                        self.editor.view_anchor_row()
                    } else {
                        None
                    },
                    wrapcolumn: window.display.wrapcolumn,
                }
            })
            .collect()
    }

    /// Clear one-shot scroll requests after their frame has been composed.
    pub fn clear_scroll_requests(&mut self) {
        self.editor.clear_scroll_request();
    }

    pub fn handle_key(&mut self, key: Key) -> io::Result<Outcome> {
        let action = self
            .editor
            .feed_keys(vec![key])
            .map_err(|e| invalid(e.to_string()))?;
        let mut action = action;
        let mut outcome = Outcome::Continue;
        while action != EditorAction::Continue {
            let plan = self.begin_action(action)?;
            let next = self.drive_without_events(plan)?;
            if next == Outcome::Quit {
                return Ok(next);
            }
            if next != Outcome::Continue {
                outcome = next;
            }
            action = self
                .editor
                .next_queued()
                .map_err(|e| invalid(e.to_string()))?;
        }
        Ok(outcome)
    }

    pub fn paste(&mut self, bytes: &[u8]) -> io::Result<Outcome> {
        self.editor
            .paste(bytes)
            .map_err(|e| invalid(e.to_string()))?;
        Ok(Outcome::Continue)
    }

    pub fn command(&mut self, command: &str) -> io::Result<Outcome> {
        let plan = self.begin_command(command)?;
        let outcome = self.drive_without_events(plan)?;
        self.finish_command();
        Ok(outcome)
    }

    pub fn initial_command(&mut self, command: &str) -> io::Result<Outcome> {
        if command.is_empty() {
            return Ok(Outcome::Continue);
        }
        if command.starts_with('/') || command.starts_with('?') {
            for character in command.chars() {
                self.handle_key(Key::Char(character))?;
            }
            self.handle_key(Key::Enter)
        } else {
            self.command(command.strip_prefix(':').unwrap_or(command))
        }
    }

    pub fn open_cli_path(
        &mut self,
        path: &Path,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> io::Result<()> {
        let plan = self.begin_open(path, input, read_only)?;
        self.drive_without_events(plan).map(|_| ())
    }

    fn apply_native(&mut self, action: EditorAction) -> io::Result<Outcome> {
        match action {
            EditorAction::Continue => Ok(Outcome::Continue),
            EditorAction::Message(message) => Ok(Outcome::Message(message)),
            EditorAction::HostCommand(command) => self.host_command(&command),
            _ => Err(invalid(
                "lifecycle and callback actions require a host plan",
            )),
        }
    }

    pub fn save(&mut self, path: Option<&Path>, force: bool) -> io::Result<()> {
        self.save_ranges(path, force, None, false).map(|_| ())
    }

    fn save_ranges(
        &mut self,
        path: Option<&Path>,
        force: bool,
        ranges: Option<&[std::ops::Range<usize>]>,
        visual: bool,
    ) -> io::Result<usize> {
        let buffer = self.windows[self.active].buffer;
        self.save_buffer_ranges(buffer, path, force, ranges, visual)
    }

    fn save_buffer_ranges(
        &mut self,
        buffer: usize,
        path: Option<&Path>,
        force: bool,
        ranges: Option<&[std::ops::Range<usize>]>,
        visual: bool,
    ) -> io::Result<usize> {
        let text = self
            .with_buffer(buffer, |text| text.read_snapshot())
            .ok_or_else(|| invalid("closed file"))?;
        let entire = ranges.is_none_or(|rs| rs.len() == 1 && rs[0] == (0..text.len()));
        if path.is_none() && !force && (visual || !entire) {
            return Err(invalid(
                "file would be reduced to the selection or range; use :w! to override",
            ));
        }
        let snapshot = self
            .with_buffer(buffer, |text| {
                if let Some(ranges) = ranges {
                    text.read_ranges(ranges)
                } else {
                    Ok(text.read_snapshot())
                }
            })
            .expect("live buffer")
            .map_err(|e| invalid(e.to_string()))?;
        let metadata = self.buffers[buffer].as_ref().expect("live buffer");
        if metadata.read_only && !force {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is read-only; use :w! to override",
            ));
        }
        let method = metadata.save_method;
        let path = path.map(|p| self.resolve_path(p));
        let same = path.as_ref().is_some_and(|p| {
            self.buffer_path(buffer)
                .is_some_and(|current| Self::same_file(&current, p))
        });
        let mut new_document = if let Some(path) = path.filter(|_| !same) {
            if self.find_buffer(&path).is_some() && !force {
                return Err(invalid("destination is open in another buffer"));
            }
            if path.try_exists()? && !force {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "destination exists; use :w! to overwrite",
                ));
            }
            Some(Document::open(path)?)
        } else {
            None
        };
        let existing = new_document.is_none();
        let mut target = if existing {
            self.buffers[buffer]
                .as_mut()
                .expect("live buffer")
                .document
                .take()
        } else {
            new_document.take()
        }
        .ok_or_else(|| invalid("no file name; use :w FILE"))?;
        let result = self
            .with_buffer_mut(buffer, |text| {
                std::mem::swap(text, &mut target.text);
                let result = target.save_snapshot(&snapshot, method, force);
                std::mem::swap(text, &mut target.text);
                result
            })
            .expect("live buffer");
        // Restore the original Document even on failure, including its conflict baseline.
        if existing
            || result.is_ok()
                && self.buffers[buffer]
                    .as_ref()
                    .expect("live buffer")
                    .document
                    .is_none()
        {
            self.buffers[buffer].as_mut().expect("live buffer").document = Some(target);
        }
        result?;
        Ok(snapshot.len())
    }

    fn closable(&self, index: usize) -> bool {
        let buffer = self.windows[index].buffer;
        self.buffers[buffer]
            .as_ref()
            .is_some_and(|buffer| buffer.internal)
            || !self.text(buffer).modified()
            || self
                .windows
                .iter()
                .filter(|window| window.buffer == buffer)
                .count()
                > 1
    }

    fn host_command(&mut self, command: &str) -> io::Result<Outcome> {
        let words = command_words(command)?;
        let Some(name) = words.first() else {
            return Ok(Outcome::Continue);
        };
        let name = name.trim_end_matches('!');
        let args = &words[1..];
        match name {
            "set" => {
                let Some(option) = args.first() else {
                    return Err(invalid("expected set option [value]"));
                };
                if args.len() > 2 {
                    return Err(invalid("expected set option [value]"));
                }
                let (raw, equal_value) = option
                    .split_once('=')
                    .map_or((option.as_str(), None), |(name, value)| (name, Some(value)));
                let toggle = raw.ends_with('!');
                let raw = raw.trim_end_matches('!');
                let (name, negate) = raw
                    .strip_prefix("no")
                    .map_or((raw, false), |name| (name, true));
                let value = equal_value.or_else(|| args.get(1).map(String::as_str));
                let window = self
                    .windows
                    .get(self.active)
                    .map_or(usize::MAX, |window| window.id);
                self.apply_set_option(name, value, toggle, negate, window)?;
                Ok(Outcome::Continue)
            }
            "wincmd" => match args.first().map(String::as_str) {
                Some("w" | "j" | "l") => {
                    self.focus((self.active + 1) % self.windows.len())?;
                    Ok(Outcome::Continue)
                }
                Some("p" | "k" | "h") => {
                    self.focus((self.active + self.windows.len() - 1) % self.windows.len())?;
                    Ok(Outcome::Continue)
                }
                _ => Err(invalid("unknown window command")),
            },
            "b" | "buffer" => {
                let target = args
                    .first()
                    .ok_or_else(|| invalid("expected buffer number or name"))?;
                let index = if let Ok(number) = target.parse::<usize>() {
                    self.windows
                        .iter()
                        .position(|window| window.buffer + 1 == number)
                } else {
                    let path = self.resolve_path(Path::new(target));
                    self.find_buffer(&path).and_then(|buffer| {
                        self.windows
                            .iter()
                            .position(|window| window.buffer == buffer)
                    })
                }
                .ok_or_else(|| invalid("no such buffer"))?;
                self.focus(index)?;
                Ok(Outcome::Continue)
            }
            "ls" | "files" | "B" => Ok(Outcome::Message(
                self.windows
                    .iter()
                    .map(|window| {
                        let name = self.buffers[window.buffer]
                            .as_ref()
                            .and_then(|buffer| buffer.document.as_ref())
                            .map_or_else(
                                || "[No Name]".into(),
                                |document| document.path().display().to_string(),
                            );
                        format!(
                            "{} {}{}",
                            window.buffer + 1,
                            name,
                            if self.text(window.buffer).modified() {
                                " [+]"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            "cd" | "chdir" => {
                if args.len() > 1 {
                    return Err(invalid("only one directory allowed"));
                }
                let path = args
                    .first()
                    .map(PathBuf::from)
                    .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                    .ok_or_else(|| invalid("HOME is not set"))?;
                let path = std::fs::canonicalize(self.resolve_path(&path))?;
                if !path.is_dir() {
                    return Err(invalid("not a directory"));
                }
                self.directory = path;
                self.editor.set_directory(&self.directory);
                Ok(Outcome::Continue)
            }
            "pwd" => Ok(Outcome::Message(self.directory.display().to_string())),
            _ => Err(invalid(format!("unsupported host command: {name}"))),
        }
    }
}

/// Quoted file arguments without launching a shell or interpreting substitutions.
pub fn command_words(command: &str) -> io::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut started = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => word.push(c),
            (_, '\\') => {
                word.push(chars.next().ok_or_else(|| invalid("trailing escape"))?);
                started = true;
            }
            (None, '\'' | '"') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            _ => {
                word.push(c);
                started = true;
            }
        }
    }
    if quote.is_some() {
        return Err(invalid("unterminated quote"));
    }
    if started {
        words.push(word);
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "vis-workspace-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn keys(workspace: &mut Workspace, input: &str) {
        for c in input.chars() {
            workspace.handle_key(Key::Char(c)).unwrap();
        }
    }

    #[test]
    fn shared_views_have_independent_marked_cursors_and_one_history() {
        let dir = Directory::new();
        let file = dir.file("file", b"a\nb\nc\n");
        let mut workspace = Workspace::new(&[file], None, false).unwrap();
        keys(&mut workspace, "2j");
        assert_eq!(workspace.editor.cursor, 4);
        workspace.command("split").unwrap();
        keys(&mut workspace, "ggiPREFIX");
        workspace.handle_key(Key::Escape).unwrap();
        workspace.command("wincmd p").unwrap();
        assert_eq!(workspace.editor.cursor, 10);
        assert_eq!(workspace.editor.text.to_vec(), b"PREFIXa\nb\nc\n");
        let frames = workspace.frames();
        assert!(Arc::ptr_eq(&frames[0].bytes, &frames[1].bytes));
        keys(&mut workspace, "u");
        assert_eq!(workspace.editor.text.to_vec(), b"a\nb\nc\n");
        workspace.command("wincmd w").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"a\nb\nc\n");
        assert!(workspace.editor.cursor <= workspace.editor.text.len());
    }

    #[test]
    fn registers_are_global_but_file_histories_are_independent() {
        let dir = Directory::new();
        let first = dir.file("first", b"first\n");
        let second = dir.file("second", b"second\n");
        let mut workspace = Workspace::new(&[first, second], None, false).unwrap();
        workspace.focus(1).unwrap();
        keys(&mut workspace, "\"ayy");
        workspace.command("wincmd w").unwrap();
        keys(&mut workspace, "\"ap");
        assert_eq!(workspace.editor.text.to_vec(), b"second\nfirst\n");
        workspace.command("wincmd p").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"first\n");
        assert!(!workspace.editor.text.modified());
        workspace.command("wincmd w").unwrap();
        keys(&mut workspace, "u");
        assert_eq!(workspace.editor.text.to_vec(), b"second\n");
    }

    #[test]
    fn closing_a_shared_view_preserves_unsaved_buffer_until_last_view() {
        let dir = Directory::new();
        let file = dir.file("file", b"text\n");
        let mut workspace = Workspace::new(std::slice::from_ref(&file), None, false).unwrap();
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        workspace.command("vsplit").unwrap();
        assert_eq!(workspace.command("q").unwrap(), Outcome::Continue);
        assert_eq!(workspace.window_count(), 1);
        assert!(workspace.command("q").is_err());
        assert_eq!(workspace.command("wq").unwrap(), Outcome::Quit);
        assert_eq!(std::fs::read(file).unwrap(), b"Xtext\n");
    }

    #[test]
    fn rejected_edit_and_reload_leave_buffer_and_history_intact() {
        let dir = Directory::new();
        let file = dir.file("file", b"before\n");
        let mut workspace = Workspace::new(std::slice::from_ref(&file), None, false).unwrap();
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        assert!(workspace.command("edit").is_err());
        assert!(
            workspace
                .command(&format!("edit! '{}'", dir.0.display()))
                .is_err()
        );
        assert_eq!(workspace.editor.text.to_vec(), b"Xbefore\n");
        workspace.command("split").unwrap();
        assert!(workspace.command("edit!").is_err());
        workspace.command("q").unwrap();
        workspace.command("edit!").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"before\n");
        assert!(!workspace.editor.text.modified());
        assert_eq!(
            workspace.path(),
            Some(std::fs::canonicalize(&file).unwrap().as_path())
        );
    }

    #[test]
    fn readonly_and_external_conflicts_do_not_lose_editor_ownership() {
        let dir = Directory::new();
        let file = dir.file("file", b"before");
        let mut workspace = Workspace::new(std::slice::from_ref(&file), None, true).unwrap();
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        assert!(workspace.command("w").is_err());
        assert_eq!(std::fs::read(&file).unwrap(), b"before");
        workspace.command("w!").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"Xbefore");
        let mut workspace = Workspace::new(std::slice::from_ref(&file), None, false).unwrap();
        keys(&mut workspace, "iY");
        workspace.handle_key(Key::Escape).unwrap();
        std::fs::write(&file, b"external").unwrap();
        assert!(workspace.command("w").is_err());
        assert_eq!(workspace.editor.text.to_vec(), b"YXbefore");
        assert_eq!(std::fs::read(file).unwrap(), b"external");
    }

    #[test]
    fn qall_closes_clean_windows_and_keeps_dirty_ones() {
        let dir = Directory::new();
        let first = dir.file("first", b"first");
        let second = dir.file("second", b"second");
        let mut workspace = Workspace::new(&[first, second], None, false).unwrap();
        workspace.focus(1).unwrap();
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        assert!(workspace.command("qall").is_err());
        assert_eq!(workspace.window_count(), 1);
        assert_eq!(workspace.editor.text.to_vec(), b"Xfirst");
        assert_eq!(workspace.command("qall!").unwrap(), Outcome::Quit);
    }

    #[test]
    fn stdin_window_and_quoted_file_arguments_preserve_bytes() {
        assert_eq!(
            command_words("open 'a b' \"c d\" x\\ y ''").unwrap(),
            ["open", "a b", "c d", "x y", ""]
        );
        assert!(command_words("open 'unterminated").is_err());
        let dir = Directory::new();
        let file = dir.file("a b", b"file");
        let mut workspace = Workspace::new(
            &[PathBuf::from("-"), file],
            Some(b"\0\xffstdin".to_vec()),
            false,
        )
        .unwrap();
        workspace.focus(1).unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"\0\xffstdin");
        assert!(workspace.editor.text.modified());
        workspace.command("wincmd w").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"file");
    }

    #[test]
    fn cd_changes_relative_file_reads_without_changing_process_directory() {
        let original = std::env::current_dir().unwrap();
        let dir = Directory::new();
        dir.file("input", b"from directory");
        let mut workspace = Workspace::new(&[], None, false).unwrap();
        workspace
            .command(&format!("cd '{}'", dir.0.display()))
            .unwrap();
        workspace.command("r input").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"from directory");
        assert_eq!(std::env::current_dir().unwrap(), original);
        assert_eq!(
            workspace.directory(),
            std::fs::canonicalize(&dir.0).unwrap()
        );
    }

    #[test]
    fn quoted_save_and_help_window_preserve_the_original_buffer() {
        let dir = Directory::new();
        let path = dir.0.join("saved file");
        let mut workspace = Workspace::new(&[], Some(b"contents".to_vec()), false).unwrap();
        workspace
            .command(&format!("w '{}'", path.display()))
            .unwrap();
        workspace.command("help").unwrap();
        assert_eq!(workspace.window_count(), 2);
        assert!(workspace.editor.text.to_vec().starts_with(b"vis-rs:"));
        workspace.command("q").unwrap();
        assert_eq!(workspace.editor.text.to_vec(), b"contents");
        assert_eq!(std::fs::read(path).unwrap(), b"contents");
    }

    #[test]
    fn writing_named_buffer_to_other_files_copies_without_changing_its_name() {
        let dir = Directory::new();
        let original = dir.file("original", b"original");
        let first = dir.0.join("copy one");
        let second = dir.0.join("copy two");
        let mut workspace = Workspace::new(std::slice::from_ref(&original), None, false).unwrap();
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        workspace
            .command(&format!("w '{}' '{}'", first.display(), second.display()))
            .unwrap();
        assert_eq!(std::fs::read(&original).unwrap(), b"original");
        assert_eq!(std::fs::read(first).unwrap(), b"Xoriginal");
        assert_eq!(std::fs::read(second).unwrap(), b"Xoriginal");
        assert_eq!(
            workspace.path(),
            Some(std::fs::canonicalize(&original).unwrap().as_path())
        );
        assert!(!workspace.editor.text.modified());
        workspace.command("w").unwrap();
        assert_eq!(std::fs::read(original).unwrap(), b"Xoriginal");
    }

    #[test]
    fn split_defaults_window_options_and_focus_preserves_global_autoindent() {
        let mut workspace = Workspace::new(&[], None, false).unwrap();
        workspace
            .command("set tabwidth=4 expandtab autoindent")
            .unwrap();
        workspace.command("split").unwrap();
        assert_eq!(workspace.editor.tab_width, 8);
        assert!(!workspace.editor.expand_tab && workspace.editor.auto_indent);
        workspace
            .command("set tabwidth=2 noexpandtab noautoindent")
            .unwrap();
        workspace.command("wincmd p").unwrap();
        assert_eq!(workspace.editor.tab_width, 4);
        assert!(workspace.editor.expand_tab && !workspace.editor.auto_indent);
        workspace.command("wincmd w").unwrap();
        assert_eq!(workspace.editor.tab_width, 2);
        assert!(!workspace.editor.expand_tab && !workspace.editor.auto_indent);
        let frames = workspace.frames();
        assert_eq!((frames[0].tab_width, frames[1].tab_width), (2, 4));
    }

    #[test]
    fn aliases_of_new_files_share_the_same_buffer() {
        let dir = Directory::new();
        let path = dir.0.join("new file");
        let alias = dir.0.join(".").join("new file");
        let workspace = Workspace::new(&[path, alias], None, false).unwrap();
        let frames = workspace.frames();
        assert!(Arc::ptr_eq(&frames[0].bytes, &frames[1].bytes));
    }

    #[test]
    fn deleted_inactive_marks_round_to_character_boundaries_before_focus() {
        let dir = Directory::new();
        let file = dir.file("file", b"abcd\n");
        let mut workspace = Workspace::new(&[file], None, false).unwrap();
        keys(&mut workspace, "2l");
        workspace.command("split").unwrap();
        workspace.command(",c/😊/").unwrap();
        workspace.command("wincmd p").unwrap();
        assert_eq!(workspace.editor.cursor, 0);
        assert_eq!(workspace.editor.text.to_vec(), "😊".as_bytes());
        workspace.command("wincmd w").unwrap();
        let active = workspace.active_window_id();
        let cursor = workspace.editor.cursor;
        let previous = (workspace.active + workspace.windows.len() - 1) % workspace.windows.len();
        workspace.windows[previous].view.primary = 99;
        assert!(workspace.command("wincmd p").is_err());
        assert_eq!(workspace.active_window_id(), active);
        assert_eq!(workspace.editor.cursor, cursor);
        assert_eq!(workspace.editor.text.to_vec(), "😊".as_bytes());
    }

    #[test]
    fn visual_and_addressed_writes_project_ranges_without_editing_the_buffer() {
        let dir = Directory::new();
        let file = dir.file("file", b"abcdef\n");
        let copy = dir.0.join("copy");
        let lines = dir.0.join("lines");
        let mut workspace = Workspace::new(std::slice::from_ref(&file), None, false).unwrap();
        keys(&mut workspace, "v2l");
        workspace
            .command(&format!("w '{}'", copy.display()))
            .unwrap();
        assert_eq!(std::fs::read(copy).unwrap(), b"abc");
        assert_eq!(workspace.editor.text.to_vec(), b"abcdef\n");
        assert!(workspace.command("w").is_err());
        workspace.command("w!").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"abc");
        workspace.handle_key(Key::Escape).unwrap();
        workspace.command("w").unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), b"abcdef\n");
        workspace
            .command(&format!("#1,#3w '{}'", lines.display()))
            .unwrap();
        assert_eq!(std::fs::read(lines).unwrap(), b"bc");
        assert_eq!(workspace.editor.text.to_vec(), b"abcdef\n");
    }

    #[cfg(unix)]
    #[test]
    fn normalization_preserves_a_final_symlink_for_save_policy() {
        let dir = Directory::new();
        let target = dir.file("target", b"target");
        let link = dir.0.join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let mut workspace = Workspace::new(std::slice::from_ref(&link), None, false).unwrap();
        assert_eq!(
            workspace.path().unwrap().file_name(),
            Some(std::ffi::OsStr::new("link"))
        );
        keys(&mut workspace, "iX");
        workspace.handle_key(Key::Escape).unwrap();
        workspace.command("w").unwrap();
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(target).unwrap(), b"Xtarget");
    }
}

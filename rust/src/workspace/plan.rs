//! Owned host continuations. Each event is yielded with no workspace borrow held.
use super::*;
use std::collections::VecDeque;
use std::ops::Range;

/// Typed parser/validation/transcript rejection, distinct from a handler's
/// ordinary execution failure and from an accepted evaluation diagnostic.
pub fn command_rejected(error: &io::Error) -> bool {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<crate::sam::SamError>())
        .is_some_and(|error| error.phase() != crate::sam::SamErrorPhase::Evaluation)
}

#[derive(Clone, Debug)]
pub struct SavePlan {
    pub buffer: usize,
    pub path: PathBuf,
    pub force: bool,
    pub ranges: Option<Vec<Range<usize>>>,
}

pub enum HostStep {
    Input(crate::editor::InputRequest),
    Callback(crate::editor::CallbackRequest),
    Command(String),
    TargetCommand {
        token: u64,
        request: crate::sam::HostRequest,
    },
    PromptCommand {
        token: u64,
        command: String,
    },
    FileOpen(usize),
    WindowOpen(usize),
    WindowClose(usize),
    FileClose(usize),
    SavePre(SavePlan),
    SavePost {
        buffer: usize,
        path: PathBuf,
    },
    Complete(Outcome),
}

struct SaveOperation {
    public: SavePlan,
    explicit: bool,
    visual: bool,
    whole: bool,
    stdout: bool,
}

struct WriteAction {
    window: usize,
    quit: bool,
    force: bool,
    paths: Option<Vec<String>>,
    ranges: Option<Vec<Range<usize>>>,
    visual: bool,
}

enum Operation {
    Reload {
        window: usize,
        path: PathBuf,
        force_new: bool,
    },
    ReloadClose {
        window: usize,
        buffer: usize,
        old: usize,
    },
    ReloadInstall {
        window: usize,
        buffer: usize,
    },
    Open {
        path: Option<PathBuf>,
        input: Option<Vec<u8>>,
        read_only: bool,
        copy: Option<usize>,
        split: Option<(usize, bool)>,
        layout: Layout,
        reload: bool,
    },
    AddWindow {
        buffer: usize,
        view: ViewState,
        layout: Layout,
        display: WindowDisplay,
        split: Option<(usize, bool)>,
    },
    Close {
        window: usize,
        force: bool,
    },
    CloseFile {
        window: usize,
    },
    RemoveWindow {
        window: usize,
    },
    CopySplit {
        source: usize,
        target: usize,
        shared: bool,
    },
    FinishEdit {
        old: usize,
    },
    FocusWindow(usize),
    Structural,
    ChangeDirectory(Option<PathBuf>),
    Save(SaveOperation),
    CommitSave(SaveOperation),
    Native(EditorAction),
    Command(String),
    Help,
    Suspend,
    Message(Vec<u8>),
    AppendMessage {
        window: usize,
        bytes: Vec<u8>,
    },
    FinishError(String),
    ShowPrompt(Vec<u8>),
    SubmitPrompt,
    CancelPrompt,
    ExpandPrompt,
    FinishPrompt {
        submission: prompt::Submission,
        success: bool,
    },
}

pub struct HostPlan {
    operations: VecDeque<Operation>,
    closing: Vec<usize>,
    opening: Vec<usize>,
    outcome: Outcome,
    callback_windows: BTreeMap<u64, usize>,
    callbacks: BTreeSet<u64>,
    inputs: BTreeSet<u64>,
    prompts: BTreeMap<u64, prompt::Submission>,
    structural: Option<structural::Execution>,
    child: Option<Box<HostPlan>>,
}
impl HostPlan {
    fn new(operations: impl IntoIterator<Item = Operation>) -> Self {
        Self {
            operations: operations.into_iter().collect(),
            closing: Vec::new(),
            opening: Vec::new(),
            outcome: Outcome::Continue,
            callback_windows: BTreeMap::new(),
            callbacks: BTreeSet::new(),
            inputs: BTreeSet::new(),
            prompts: BTreeMap::new(),
            structural: None,
            child: None,
        }
    }
}

impl Workspace {
    fn begin_structural(
        &mut self,
        command: &str,
        compatible: bool,
    ) -> io::Result<Option<HostPlan>> {
        let names = self.editor.external_command_names();
        let parsed = self
            .editor
            .sam_session_mut()
            .parse_workspace_program(command, &names);
        if !matches!(&parsed, Ok(None)) {
            self.editor.sync_sam_registers();
        }
        let Some(program) = parsed.map_err(io::Error::other)? else {
            return Ok(None);
        };
        self.next_structural = self
            .next_structural
            .checked_add(1)
            .ok_or_else(|| invalid("structural token limit exceeded"))?;
        let window = self
            .windows
            .get(self.active)
            .map_or(usize::MAX, |window| window.id);
        let mut plan = HostPlan::new([Operation::Structural]);
        plan.structural = Some(structural::Execution::new(
            self.next_structural,
            program,
            window,
            compatible,
            self.editor.cursor,
        ));
        Ok(Some(plan))
    }

    pub fn resume_target_command(
        &mut self,
        plan: &mut HostPlan,
        token: u64,
        success: bool,
    ) -> io::Result<()> {
        if let Some(child) = plan.child.as_mut() {
            return self.resume_target_command(child, token, success);
        }
        let execution = plan
            .structural
            .as_mut()
            .filter(|execution| execution.token == token && execution.waiting)
            .ok_or_else(|| invalid("unknown or completed target command"))?;
        execution.waiting = false;
        execution.host_result(success);
        execution
            .continuation
            .resume_host(success)
            .map_err(io::Error::other)?;
        plan.operations.push_front(Operation::Structural);
        Ok(())
    }

    fn begin_sam_host(
        &mut self,
        request: &crate::sam::HostRequest,
    ) -> io::Result<Option<HostPlan>> {
        use crate::sam::HostCommand as H;
        let target = request.window;
        let file = self
            .window_file_id(target)
            .filter(|file| *file == request.file)
            .ok_or_else(|| invalid("structural target was closed or reloaded"))?;
        let open = |path, layout, copy, split| Operation::Open {
            path,
            input: None,
            read_only: false,
            copy,
            split,
            layout,
            reload: false,
        };
        let mut operations = Vec::new();
        match request.command {
            H::Write | H::WriteQuit => {
                return self
                    .plan_write(WriteAction {
                        window: target,
                        quit: request.command == H::WriteQuit,
                        force: request.force,
                        paths: (!request.args.is_empty()).then(|| request.args.clone()),
                        ranges: Some(if request.visual {
                            request.ranges.clone()
                        } else {
                            vec![request.range.clone()]
                        }),
                        visual: request.visual,
                    })
                    .map(Some);
            }
            H::Quit | H::BufferDelete => operations.push(Operation::Close {
                window: target,
                force: request.force,
            }),
            H::QuitAll => {
                let mut dirty = false;
                for (index, window) in self.windows.iter().enumerate() {
                    if self.buffer_internal(window.buffer) == Some(true) {
                        continue;
                    }
                    if request.force || self.closable(index) && !self.text(window.buffer).modified()
                    {
                        operations.push(Operation::Close {
                            window: window.id,
                            force: request.force,
                        });
                    } else {
                        dirty = true;
                    }
                }
                if dirty {
                    operations.push(Operation::FinishError(
                        "unsaved changes; use :w or :qall!".into(),
                    ));
                }
            }
            H::Edit => {
                if request.args.len() > 1 {
                    return Err(invalid("only one file allowed"));
                }
                let index = self
                    .windows
                    .iter()
                    .position(|window| window.id == target)
                    .expect("live target");
                if !request.force && !self.closable(index) {
                    return Err(invalid("unsaved changes; use :w or :e!"));
                }
                if let Some(path) = request.args.first() {
                    operations.push(open(Some(PathBuf::from(path)), self.layout, None, None));
                    operations.push(Operation::FinishEdit { old: target });
                } else {
                    if self
                        .windows
                        .iter()
                        .filter(|window| window.buffer == file)
                        .count()
                        > 1
                    {
                        return Err(invalid("cannot reload a file opened in multiple windows"));
                    }
                    let path = self
                        .buffer_path(file)
                        .ok_or_else(|| invalid("no file name"))?;
                    operations.push(Operation::Reload {
                        window: target,
                        path,
                        force_new: true,
                    });
                }
            }
            H::Open | H::New | H::VNew => {
                let layout = if request.command == H::New {
                    Layout::Horizontal
                } else if request.command == H::VNew {
                    Layout::Vertical
                } else {
                    self.layout
                };
                if request.args.is_empty() || request.command != H::Open {
                    operations.push(open(None, layout, None, None));
                } else {
                    for path in &request.args {
                        operations.push(open(Some(PathBuf::from(path)), layout, None, None));
                    }
                }
            }
            H::Split | H::VSplit => {
                if request.args.len() > 1 {
                    return Err(invalid("only one file allowed"));
                }
                let shared = request.args.is_empty();
                operations.push(open(
                    request.args.first().map(PathBuf::from),
                    if request.command == H::Split {
                        Layout::Horizontal
                    } else {
                        Layout::Vertical
                    },
                    shared.then_some(target),
                    Some((target, shared)),
                ));
            }
            H::ChangeDirectory => {
                if request.args.len() > 1 {
                    return Err(invalid("only one directory allowed"));
                }
                operations.push(Operation::ChangeDirectory(
                    request.args.first().map(PathBuf::from),
                ));
            }
            H::Help => operations.push(Operation::Help),
            H::WorkingDirectory => operations.push(Operation::Native(EditorAction::Message(
                self.directory.display().to_string(),
            ))),
            H::Files => {
                let listing = self
                    .windows
                    .iter()
                    .filter(|window| self.buffer_internal(window.buffer) != Some(true))
                    .map(|window| {
                        format!(
                            "{} {}{}",
                            window.buffer + 1,
                            self.buffer_path(window.buffer).map_or_else(
                                || "[No Name]".to_string(),
                                |path| path.display().to_string()
                            ),
                            if self.text(window.buffer).modified() {
                                " [+]"
                            } else {
                                ""
                            }
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                operations.push(Operation::Native(EditorAction::Message(listing)));
            }
            H::Buffer => {
                let name = request
                    .args
                    .first()
                    .ok_or_else(|| invalid("expected buffer number or name"))?;
                let target = if let Ok(number) = name.parse::<usize>() {
                    self.windows
                        .iter()
                        .find(|window| window.buffer + 1 == number)
                        .map(|window| window.id)
                } else {
                    self.find_buffer(&self.resolve_path(Path::new(name)))
                        .and_then(|file| {
                            self.windows
                                .iter()
                                .find(|window| window.buffer == file)
                                .map(|window| window.id)
                        })
                }
                .ok_or_else(|| invalid("no such buffer"))?;
                operations.push(Operation::FocusWindow(target));
            }
            H::WindowCommand => {
                return Err(invalid(
                    "window command is unavailable inside a structural program",
                ));
            }
            H::Set
            | H::Map
            | H::Unmap
            | H::MapWindow
            | H::UnmapWindow
            | H::LangMap
            | H::Earlier
            | H::Later
            | H::Substitute
            | H::User(_) => return Ok(None),
        }
        Ok(Some(HostPlan::new(operations)))
    }

    /// An initialized host without a file: Lua INIT observes precisely this state.
    pub fn empty() -> io::Result<Self> {
        let mut editor = Editor::new(Text::new());
        editor.set_host_prompts(true);
        Ok(Self {
            editor,
            // Native vis creates its persistent prompt and message files before
            // Lua INIT; they are iterable files but emit no lifecycle events.
            buffers: vec![
                Some(Buffer {
                    text: Text::new(),
                    state: BufferState::default(),
                    document: None,
                    read_only: false,
                    stdout: false,
                    save_method: SaveMethod::Auto,
                    internal: true,
                }),
                Some(Buffer {
                    text: Text::new(),
                    state: BufferState::default(),
                    document: None,
                    read_only: true,
                    stdout: false,
                    save_method: SaveMethod::Auto,
                    internal: true,
                }),
            ],
            windows: Vec::new(),
            active: 0,
            next_window: 0,
            directory: std::env::current_dir()?,
            layout: Layout::Horizontal,
            closing: BTreeSet::new(),
            opening: BTreeSet::new(),
            message_buffer: Some(1),
            message_window: None,
            prompt_buffer: Some(0),
            next_prompt: 0,
            executing_prompts: BTreeMap::new(),
            next_structural: 0,
        })
    }
    pub fn begin_key(&mut self, key: Key) -> io::Result<HostPlan> {
        let action = self
            .editor
            .handle_key(key)
            .map_err(|e| invalid(e.to_string()))?;
        self.begin_action(action)
    }
    pub fn begin_command(&mut self, command: &str) -> io::Result<HostPlan> {
        if let Some(plan) = self.begin_structural(command, false)? {
            return Ok(plan);
        }
        let action = self
            .editor
            .execute_command(command)
            .map_err(|e| invalid(e.to_string()))?;
        self.begin_action(action)
    }

    /// Prepare the command language's native boolean result. Once parsing,
    /// validation and transcript generation succeed, handler failures are user
    /// messages rather than rejected commands. `begin_command` remains strict
    /// for Rust callers that need the execution error itself.
    pub fn prepare_command(&mut self, command: &str) -> io::Result<HostPlan> {
        if let Some(plan) = self.begin_structural(command, true)? {
            return Ok(plan);
        }
        let action = self
            .editor
            .prepare_command(command)
            .map_err(|error| invalid(error.to_string()))?;
        Ok(match self.begin_action(action) {
            Ok(plan) => plan,
            Err(error) => {
                HostPlan::new([Operation::Native(EditorAction::Message(error.to_string()))])
            }
        })
    }
    pub fn begin_open(
        &mut self,
        path: &Path,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> io::Result<HostPlan> {
        Ok(HostPlan::new([Operation::Open {
            path: Some(path.into()),
            input,
            read_only,
            copy: None,
            split: None,
            layout: self.layout,
            reload: false,
        }]))
    }
    pub fn begin_message(&mut self, bytes: Vec<u8>) -> io::Result<HostPlan> {
        Ok(HostPlan::new([Operation::Message(bytes)]))
    }
    pub fn begin_scratch(
        &mut self,
        input: Option<Vec<u8>>,
        read_only: bool,
    ) -> io::Result<HostPlan> {
        Ok(HostPlan::new([Operation::Open {
            path: None,
            input,
            read_only,
            copy: None,
            split: None,
            layout: self.layout,
            reload: false,
        }]))
    }
    pub fn begin_change_window_file(&mut self, window: usize, path: &Path) -> io::Result<HostPlan> {
        if self.window_file_id(window).is_none() {
            return Err(invalid("closed window"));
        }
        Ok(HostPlan::new([Operation::Reload {
            window,
            path: path.into(),
            force_new: false,
        }]))
    }
    pub fn begin_close_window(&mut self, window: usize, force: bool) -> io::Result<HostPlan> {
        if !self.windows.iter().any(|w| w.id == window) {
            return Err(invalid("closed window"));
        }
        Ok(HostPlan::new([Operation::Close { window, force }]))
    }
    pub fn begin_action(&mut self, action: EditorAction) -> io::Result<HostPlan> {
        self.begin_action_at(
            self.windows.get(self.active).map(|window| window.id),
            action,
        )
    }

    /// Build a native file/window action for an explicit target without changing
    /// the global focused window observed by lifecycle callbacks.
    pub fn begin_target_action(
        &mut self,
        window: usize,
        action: EditorAction,
    ) -> io::Result<HostPlan> {
        if self.window_file_id(window).is_none() {
            return Err(invalid("closed window"));
        }
        self.begin_action_at(Some(window), action)
    }

    fn begin_action_at(
        &mut self,
        target_window: Option<usize>,
        action: EditorAction,
    ) -> io::Result<HostPlan> {
        let mut operations = Vec::new();
        match action {
            EditorAction::PromptShow { title } => operations.push(Operation::ShowPrompt(title)),
            EditorAction::PromptSubmit => operations.push(Operation::SubmitPrompt),
            EditorAction::PromptCancel => operations.push(Operation::CancelPrompt),
            EditorAction::PromptHistoryExpand => operations.push(Operation::ExpandPrompt),
            EditorAction::Quit { force } => {
                if let Some(window) = target_window {
                    operations.push(Operation::Close { window, force });
                }
            }
            EditorAction::Save {
                quit,
                force,
                path,
                ranges,
                visual,
            } => {
                return self.plan_write(WriteAction {
                    window: target_window.ok_or_else(|| invalid("no window"))?,
                    quit,
                    force,
                    paths: path.as_deref().map(command_words).transpose()?,
                    ranges,
                    visual,
                });
            }
            EditorAction::HostCommand(ref command) => {
                let words = command_words(command)?;
                let name = words.first().map(String::as_str).unwrap_or("");
                if words.is_empty() {
                    return Ok(HostPlan::new([]));
                }
                let force = name.ends_with('!');
                let name = name.trim_end_matches('!');
                let args = &words[1..];
                let current = target_window;
                let open = |path: Option<PathBuf>, copy, layout, reload| Operation::Open {
                    path,
                    input: None,
                    read_only: false,
                    copy,
                    split: copy.map(|source| (source, true)),
                    layout,
                    reload,
                };
                match name {
                    "set" => {
                        // The native window scoped options are applied by the
                        // workspace, which owns the window display.
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
                    }
                    "help" => operations.push(Operation::Help),
                    "suspend" => operations.push(Operation::Suspend),
                    "split" | "vsplit" => {
                        if args.len() > 1 {
                            return Err(invalid("only one file allowed"));
                        }
                        let mut operation = open(
                            args.first().map(PathBuf::from),
                            if args.is_empty() { current } else { None },
                            if name == "vsplit" {
                                Layout::Vertical
                            } else {
                                Layout::Horizontal
                            },
                            false,
                        );
                        if let Operation::Open { split, .. } = &mut operation {
                            *split = current.map(|source| (source, args.is_empty()));
                        }
                        operations.push(operation);
                    }
                    "new" | "vnew" | "open" | "o" => {
                        if name == "new" || name == "vnew" || args.is_empty() {
                            operations.push(open(
                                None,
                                None,
                                if name == "vnew" {
                                    Layout::Vertical
                                } else if name == "new" {
                                    Layout::Horizontal
                                } else {
                                    self.layout
                                },
                                false,
                            ));
                        } else {
                            for path in args {
                                operations.push(open(Some(path.into()), None, self.layout, false));
                            }
                        }
                    }
                    "e" | "edit" => {
                        if args.len() > 1 {
                            return Err(invalid("only one file allowed"));
                        }
                        let id = current.ok_or_else(|| invalid("no window"))?;
                        let index = self
                            .windows
                            .iter()
                            .position(|window| window.id == id)
                            .ok_or_else(|| invalid("closed window"))?;
                        if !force && !self.closable(index) {
                            return Err(invalid("unsaved changes; use :w or :e!"));
                        }
                        let reload = args.is_empty();
                        if reload
                            && self
                                .windows
                                .iter()
                                .filter(|w| w.buffer == self.windows[index].buffer)
                                .count()
                                > 1
                        {
                            return Err(invalid("cannot reload a file opened in multiple windows"));
                        }
                        let path = args
                            .first()
                            .map(PathBuf::from)
                            .or_else(|| self.buffer_path(self.windows[index].buffer))
                            .ok_or_else(|| invalid("no file name"))?;
                        if reload {
                            operations.push(Operation::Reload {
                                window: id,
                                path,
                                force_new: true,
                            });
                        } else {
                            operations.push(open(Some(path), None, self.layout, false));
                            operations.push(Operation::FinishEdit { old: id });
                        }
                    }
                    "qall" => {
                        let mut dirty = false;
                        for (index, w) in self.windows.iter().enumerate() {
                            if self.buffers[w.buffer]
                                .as_ref()
                                .is_some_and(|buffer| buffer.internal)
                            {
                                continue;
                            }
                            if force || self.closable(index) && !self.text(w.buffer).modified() {
                                operations.push(Operation::Close {
                                    window: w.id,
                                    force,
                                });
                            } else {
                                dirty = true;
                            }
                        }
                        if dirty {
                            operations.push(Operation::FinishError(
                                "unsaved changes; use :w or :qall!".into(),
                            ));
                        }
                    }
                    "D" | "bdelete" => {
                        if let Some(window) = current {
                            operations.push(Operation::Close { window, force });
                        }
                    }
                    "wincmd" => match args.first().map(String::as_str) {
                        Some("c") => {
                            if let Some(window) = current {
                                operations.push(Operation::Close { window, force });
                            }
                        }
                        Some("s" | "v") => operations.push(open(
                            None,
                            current,
                            if args[0] == "v" {
                                Layout::Vertical
                            } else {
                                Layout::Horizontal
                            },
                            false,
                        )),
                        Some("o") => {
                            let current_index = current
                                .and_then(|id| {
                                    self.windows.iter().position(|window| window.id == id)
                                })
                                .ok_or_else(|| invalid("no window"))?;
                            if !force
                                && self.windows.iter().enumerate().any(|(i, w)| {
                                    i != current_index
                                        && self.text(w.buffer).modified()
                                        && w.buffer != self.windows[current_index].buffer
                                })
                            {
                                return Err(invalid("unsaved changes in another window"));
                            }
                            for w in &self.windows {
                                if Some(w.id) != current {
                                    operations.push(Operation::Close {
                                        window: w.id,
                                        force: true,
                                    });
                                }
                            }
                        }
                        _ => operations.push(Operation::Native(action)),
                    },
                    "b" | "buffer" | "ls" | "files" | "B" | "cd" | "chdir" | "pwd" => {
                        operations.push(Operation::Native(action))
                    }
                    _ => operations.push(Operation::Command(command.clone())),
                }
            }
            action => operations.push(Operation::Native(action)),
        }
        Ok(HostPlan::new(operations))
    }

    fn plan_write(&mut self, request: WriteAction) -> io::Result<HostPlan> {
        let WriteAction {
            window,
            quit,
            force,
            paths,
            ranges,
            visual,
        } = request;
        let buffer = self
            .window_file_id(window)
            .ok_or_else(|| invalid("closed window"))?;
        let target = self.buffers[buffer].as_ref().expect("live buffer");
        let stdout = quit && paths.is_none() && target.document.is_none() && target.stdout;
        let untouched = quit
            && paths.is_none()
            && target.document.is_none()
            && !self.text(buffer).modified()
            && !stdout;
        let mut operations = Vec::new();
        if !untouched {
            let explicit = paths.is_some();
            let paths = if let Some(paths) = paths {
                if paths.is_empty() || paths.iter().any(String::is_empty) {
                    return Err(invalid("expected file name"));
                }
                paths
                    .into_iter()
                    .map(|path| self.resolve_path(Path::new(&path)))
                    .collect::<Vec<_>>()
            } else if stdout {
                vec![PathBuf::from("-")]
            } else {
                vec![
                    self.buffer_path(buffer)
                        .ok_or_else(|| invalid("no file name; use :w FILE"))?,
                ]
            };
            let whole = ranges.as_ref().is_none_or(|ranges| {
                ranges.len() == 1 && ranges[0] == (0..self.text(buffer).len())
            });
            if !explicit && !force && (visual || !whole) {
                return Err(invalid(
                    "file would be reduced to the selection or range; use :w! to override",
                ));
            }
            for path in paths {
                operations.push(Operation::Save(SaveOperation {
                    public: SavePlan {
                        buffer,
                        path,
                        force,
                        ranges: ranges.clone(),
                    },
                    explicit,
                    visual,
                    whole,
                    stdout,
                }));
            }
        }
        if quit {
            operations.push(Operation::Close { window, force });
        }
        Ok(HostPlan::new(operations))
    }
    pub fn resume_input(
        &mut self,
        plan: &mut HostPlan,
        token: u64,
        consumed: bool,
    ) -> io::Result<()> {
        if let Some(child) = plan.child.as_mut() {
            return self.resume_input(child, token, consumed);
        }
        plan.inputs.remove(&token);
        if self.windows.is_empty() {
            self.editor.cancel_input(token);
            return Ok(());
        }
        let action = self
            .editor
            .resume_input(token, consumed)
            .map_err(|e| invalid(e.to_string()))?;
        let next = self.begin_action(action)?;
        for operation in next.operations.into_iter().rev() {
            plan.operations.push_front(operation);
        }
        Ok(())
    }
    pub fn resume_callback(
        &mut self,
        plan: &mut HostPlan,
        token: u64,
        response: crate::editor::CallbackResponse,
    ) -> io::Result<()> {
        if let Some(child) = plan.child.as_mut() {
            return self.resume_callback(child, token, response);
        }
        plan.callbacks.remove(&token);
        let original = plan.callback_windows.remove(&token);
        let chosen = self.windows.get(self.active).map(|w| w.id);
        if let Some(original) = original {
            if !self.windows.iter().any(|w| w.id == original) {
                self.editor.cancel_callback(token);
                return Ok(());
            }
            if Some(original) != chosen {
                self.focus_window(original)?;
            }
        }
        let result = self.editor.resume_callback(token, response);
        if let Some(chosen) = chosen.filter(|id| self.windows.iter().any(|w| w.id == *id)) {
            if Some(chosen) != self.windows.get(self.active).map(|w| w.id) {
                self.focus_window(chosen)?;
            }
        }
        let action = result.map_err(|e| invalid(e.to_string()))?;
        let next = self.begin_action(action)?;
        for operation in next.operations.into_iter().rev() {
            plan.operations.push_front(operation);
        }
        Ok(())
    }
    pub fn cancel_plan(&mut self, plan: &HostPlan) {
        if let Some(child) = &plan.child {
            self.cancel_plan(child);
        }
        for submission in plan.prompts.values() {
            let _ = self.finish_prompt_submission(submission, false);
        }
        for token in &plan.callbacks {
            self.editor.cancel_callback(*token);
        }
        for token in &plan.inputs {
            self.editor.cancel_input(*token);
        }
        for operation in &plan.operations {
            match operation {
                Operation::Native(EditorAction::Callback(request)) => {
                    self.editor.cancel_callback(request.token);
                }
                Operation::Native(EditorAction::Input(request)) => {
                    self.editor.cancel_input(request.token);
                }
                Operation::FinishPrompt { submission, .. } => {
                    let _ = self.finish_prompt_submission(submission, false);
                }
                _ => {}
            }
        }
        for id in &plan.opening {
            self.opening.remove(id);
        }
        self.collect_closed_buffers();
        for id in &plan.closing {
            self.closing.remove(id);
        }
    }
    pub fn advance(&mut self, plan: &mut HostPlan, allow: bool) -> io::Result<HostStep> {
        loop {
            if let Some(mut child) = plan.child.take() {
                match self.advance(&mut child, allow) {
                    Ok(HostStep::Complete(outcome)) => {
                        if outcome != Outcome::Continue {
                            plan.outcome = outcome;
                        }
                        let execution = plan.structural.as_mut().expect("structural child owner");
                        execution
                            .continuation
                            .resume_host(true)
                            .map_err(io::Error::other)?;
                    }
                    Ok(step) => {
                        plan.child = Some(child);
                        return Ok(step);
                    }
                    Err(error) => {
                        self.cancel_plan(&child);
                        if command_rejected(&error) {
                            return Err(error);
                        }
                        plan.outcome = Outcome::Message(error.to_string());
                        let execution = plan.structural.as_mut().expect("structural child owner");
                        execution.host_error(&error);
                        execution
                            .continuation
                            .resume_host(false)
                            .map_err(io::Error::other)?;
                    }
                }
                plan.operations.push_front(Operation::Structural);
            }
            let Some(operation) = plan.operations.pop_front() else {
                return Ok(HostStep::Complete(std::mem::replace(
                    &mut plan.outcome,
                    Outcome::Continue,
                )));
            };
            match operation {
                Operation::Structural => {
                    let mut execution = plan
                        .structural
                        .take()
                        .ok_or_else(|| invalid("missing structural continuation"))?;
                    let request = self.advance_structural(&mut execution)?;
                    if let Some(message) = execution.message.take() {
                        plan.outcome = Outcome::Message(message);
                    }
                    if let Some(request) = request {
                        if matches!(
                            request.command,
                            crate::sam::HostCommand::Write | crate::sam::HostCommand::WriteQuit
                        ) {
                            execution
                                .continuation
                                .check_write(request.file)
                                .map_err(io::Error::other)?;
                        }
                        match self.begin_sam_host(&request) {
                            Ok(Some(child)) => {
                                plan.child = Some(Box::new(child));
                                plan.structural = Some(execution);
                            }
                            Ok(None) => {
                                execution.waiting = true;
                                let token = execution.token;
                                plan.structural = Some(execution);
                                return Ok(HostStep::TargetCommand { token, request });
                            }
                            Err(error) => {
                                execution.host_error(&error);
                                plan.outcome = Outcome::Message(error.to_string());
                                execution
                                    .continuation
                                    .resume_host(false)
                                    .map_err(io::Error::other)?;
                                plan.structural = Some(execution);
                                plan.operations.push_front(Operation::Structural);
                            }
                        }
                    }
                }
                Operation::ShowPrompt(title) => self.show_prompt(&title)?,
                Operation::CancelPrompt => {
                    self.cancel_prompt()?;
                    if self.external_window_ids().is_empty() {
                        plan.outcome = Outcome::Quit;
                    }
                }
                Operation::ExpandPrompt => self.expand_prompt_history()?,
                Operation::SubmitPrompt => {
                    let Some(submission) = self.prepare_prompt_submission()? else {
                        continue;
                    };
                    let token = submission.token;
                    plan.prompts.insert(token, submission.clone());
                    let command = &submission.command;
                    if command.len() < 2 {
                        self.finish_prompt_submission(&submission, true)?;
                    } else if matches!(command[0], b':' | b'+') {
                        let command = String::from_utf8_lossy(&command[1..]).into_owned();
                        self.editor
                            .set_register_slots(':', vec![command.as_bytes().to_vec()])
                            .map_err(|e| invalid(e.to_string()))?;
                        return Ok(HostStep::PromptCommand { token, command });
                    } else if matches!(command[0], b'/' | b'?') {
                        let pattern = String::from_utf8_lossy(&command[1..]);
                        match self.editor.submit_search(&pattern, command[0] == b'?') {
                            Ok(action) => {
                                let next = self.begin_action(action)?;
                                // A motion can invoke a Lua operator. Keep the
                                // original prompt alive until it completes,
                                // exactly as the synchronous native motion does.
                                plan.operations.push_front(Operation::FinishPrompt {
                                    submission,
                                    success: true,
                                });
                                for operation in next.operations.into_iter().rev() {
                                    plan.operations.push_front(operation);
                                }
                                continue;
                            }
                            Err(error) => {
                                self.finish_prompt_submission(&submission, false)?;
                                plan.outcome = Outcome::Message(error.to_string());
                            }
                        }
                    } else {
                        self.finish_prompt_submission(&submission, false)?;
                    }
                    plan.prompts.remove(&token);
                }
                Operation::FinishPrompt {
                    submission,
                    success,
                } => {
                    self.finish_prompt_submission(&submission, success)?;
                    plan.prompts.remove(&submission.token);
                    if self.external_window_ids().is_empty() {
                        plan.outcome = Outcome::Quit;
                    }
                }
                Operation::Reload {
                    window,
                    path,
                    force_new,
                } => {
                    let Some(old) = self.window_file_id(window) else {
                        continue;
                    };
                    if self.closing.contains(&window) {
                        continue;
                    }
                    let before = self.buffers.len();
                    let buffer = if force_new {
                        let mut document = Document::open(self.resolve_path(&path))?;
                        let text = std::mem::take(&mut document.text);
                        let id = self.buffers.len();
                        self.buffers.push(Some(Buffer {
                            text,
                            state: BufferState::default(),
                            document: Some(document),
                            read_only: false,
                            stdout: false,
                            save_method: SaveMethod::Auto,
                            internal: false,
                        }));
                        id
                    } else {
                        self.load(&path, false)?
                    };
                    if buffer >= before {
                        self.opening.insert(buffer);
                        plan.opening.push(buffer);
                    }
                    self.closing.insert(window);
                    plan.closing.push(window);
                    plan.operations.push_front(Operation::ReloadClose {
                        window,
                        buffer,
                        old,
                    });
                    if buffer >= before {
                        return Ok(HostStep::FileOpen(buffer));
                    }
                }
                Operation::ReloadClose {
                    window,
                    buffer,
                    old,
                } => {
                    plan.operations
                        .push_front(Operation::ReloadInstall { window, buffer });
                    if old != buffer
                        && self.window_file_id(window) == Some(old)
                        && self.windows.iter().filter(|w| w.buffer == old).count() == 1
                    {
                        return Ok(HostStep::FileClose(old));
                    }
                }
                Operation::ReloadInstall { window, buffer } => {
                    self.closing.remove(&window);
                    if plan.opening.contains(&buffer) {
                        self.opening.remove(&buffer);
                    }
                    let Some(index) = self.windows.iter().position(|w| w.id == window) else {
                        self.collect_closed_buffers();
                        continue;
                    };
                    let primary = self
                        .window_primary_selection(window)
                        .ok_or_else(|| invalid("missing primary selection"))?;
                    if index == self.active {
                        self.capture();
                        let old = self.windows[index].buffer;
                        let previous = self.buffers[old].as_mut().expect("live buffer");
                        std::mem::swap(&mut self.editor.text, &mut previous.text);
                        self.editor.swap_buffer_state(&mut previous.state);
                    }
                    let view = &mut self.windows[index].view;
                    view.selections = vec![StoredSelection {
                        id: primary,
                        mark: None,
                        fallback: Selection {
                            anchor: 0,
                            cursor: 0,
                        },
                        anchored: false,
                    }];
                    view.primary = 0;
                    view.last_visual.clear();
                    self.windows[index].buffer = buffer;
                    if index == self.active {
                        self.install_view()?;
                    }
                    self.collect_closed_buffers();
                }
                Operation::Message(mut bytes) => {
                    if let Some(nul) = bytes.iter().position(|b| *b == 0) {
                        bytes.truncate(nul);
                    }
                    bytes.push(b'\n');
                    if let Some(window) = self
                        .message_window
                        .filter(|id| self.window_file_id(*id).is_some())
                    {
                        plan.operations
                            .push_front(Operation::AppendMessage { window, bytes });
                    } else {
                        let buffer = if let Some(buffer) = self.message_buffer {
                            buffer
                        } else {
                            let buffer = self.buffers.len();
                            self.buffers.push(Some(Buffer {
                                text: Text::new(),
                                state: BufferState::default(),
                                document: None,
                                read_only: true,
                                stdout: false,
                                save_method: SaveMethod::Auto,
                                internal: true,
                            }));
                            self.message_buffer = Some(buffer);
                            buffer
                        };
                        let window = self.next_window;
                        self.message_window = Some(window);
                        plan.operations
                            .push_front(Operation::AppendMessage { window, bytes });
                        plan.operations.push_front(Operation::AddWindow {
                            buffer,
                            view: ViewState::initial(),
                            layout: self.layout,
                            display: WindowDisplay {
                                showeof: false,
                                ..WindowDisplay::default()
                            },
                            split: None,
                        });
                    }
                }
                Operation::AppendMessage { window, bytes } => {
                    let Some(buffer) = self.window_file_id(window) else {
                        continue;
                    };
                    let position = self
                        .with_buffer_mut(buffer, |text| {
                            let end = text.len();
                            text.insert(end, &bytes).map(|_| {
                                text.mark_saved();
                                end
                            })
                        })
                        .expect("live buffer")
                        .map_err(|e| invalid(e.to_string()))?;
                    self.focus_window(window)?;
                    if let Some(primary) = self.window_primary_selection(window) {
                        self.window_update_selection(
                            window,
                            primary,
                            Selection {
                                anchor: position,
                                cursor: position,
                            },
                        )?;
                    }
                }
                Operation::Suspend => plan.outcome = Outcome::Suspend,
                Operation::Help => {
                    let buffer = self.buffers.len();
                    self.buffers.push(Some(Buffer {
                        text: Text::from_bytes(HELP),
                        state: BufferState::default(),
                        document: None,
                        read_only: true,
                        stdout: false,
                        save_method: SaveMethod::Auto,
                        internal: false,
                    }));
                    plan.operations.push_front(Operation::AddWindow {
                        buffer,
                        view: ViewState::initial(),
                        layout: self.layout,
                        display: WindowDisplay::default(),
                        split: None,
                    });
                    self.opening.insert(buffer);
                    plan.opening.push(buffer);
                    return Ok(HostStep::FileOpen(buffer));
                }
                Operation::Command(command) => return Ok(HostStep::Command(command)),
                Operation::Open {
                    path,
                    input,
                    read_only,
                    copy,
                    split,
                    layout,
                    reload,
                } => {
                    let before = self.buffers.len();
                    let stdin =
                        path.as_ref().is_some_and(|p| p.as_os_str() == "-") || input.is_some();
                    let buffer = if let Some(id) = copy {
                        self.window_file_id(id)
                            .ok_or_else(|| invalid("closed source window"))?
                    } else if let Some(path) = path.filter(|p| p.as_os_str() != "-") {
                        if reload {
                            let mut doc = Document::open(self.resolve_path(&path))?;
                            let text = std::mem::take(&mut doc.text);
                            let id = self.buffers.len();
                            self.buffers.push(Some(Buffer {
                                text,
                                state: BufferState::default(),
                                document: Some(doc),
                                read_only,
                                stdout: false,
                                save_method: SaveMethod::Auto,
                                internal: false,
                            }));
                            id
                        } else {
                            self.load(&path, read_only)?
                        }
                    } else {
                        let stdout = stdin;
                        let mut text = Text::new();
                        if let Some(bytes) = input {
                            text.insert(0, &bytes).map_err(|e| invalid(e.to_string()))?;
                            text.snapshot();
                        }
                        let id = self.buffers.len();
                        self.buffers.push(Some(Buffer {
                            text,
                            state: BufferState::default(),
                            document: None,
                            read_only,
                            stdout,
                            save_method: SaveMethod::Auto,
                            internal: false,
                        }));
                        id
                    };
                    plan.operations.push_front(Operation::AddWindow {
                        buffer,
                        view: ViewState::initial(),
                        layout,
                        display: WindowDisplay {
                            showeof: !split.is_some_and(|(_, shared)| shared),
                            ..WindowDisplay::default()
                        },
                        split,
                    });
                    if buffer >= before {
                        self.opening.insert(buffer);
                        plan.opening.push(buffer);
                        return Ok(HostStep::FileOpen(buffer));
                    }
                }
                Operation::AddWindow {
                    buffer,
                    view,
                    layout,
                    display,
                    split,
                } => {
                    if self.buffers.get(buffer).and_then(Option::as_ref).is_none() {
                        continue;
                    }
                    if plan.opening.contains(&buffer) {
                        self.opening.remove(&buffer);
                    }
                    let first = self.windows.is_empty();
                    let window = self.add_window(buffer, view);
                    self.set_window_display(window, display)?;
                    self.layout = layout;
                    if first {
                        self.active = 0;
                        self.install_view()?;
                    } else {
                        self.focus_window(window)?;
                    }
                    if let Some((source, shared)) = split {
                        plan.operations.push_front(Operation::CopySplit {
                            source,
                            target: window,
                            shared,
                        });
                    }
                    return Ok(HostStep::WindowOpen(window));
                }
                Operation::CopySplit {
                    source,
                    target,
                    shared,
                } => {
                    let Some(display) = self.window_display(source) else {
                        continue;
                    };
                    if self.window_file_id(target).is_none() {
                        continue;
                    }
                    if shared {
                        self.editor
                            .copy_window_mappings(source as u64, target as u64);
                    }
                    self.set_window_display(target, display)?;
                    if shared {
                        let position =
                            self.window_primary_selection(source).and_then(|selection| {
                                self.window_selection_position(source, selection)
                            });
                        if let Some(position) = position {
                            if let Some(primary) = self.window_primary_selection(target) {
                                let anchor = if self.window_selection_anchored(target, primary)
                                    == Some(true)
                                {
                                    self.window_identified_selections(target)
                                        .and_then(|selections| {
                                            selections.into_iter().find(|(id, _)| *id == primary)
                                        })
                                        .map_or(position, |(_, selection)| selection.anchor)
                                } else {
                                    position
                                };
                                self.window_update_selection(
                                    target,
                                    primary,
                                    Selection {
                                        anchor,
                                        cursor: position,
                                    },
                                )?;
                            }
                        }
                    }
                }
                Operation::FinishEdit { old } => {
                    let Some(new) = self.windows.get(self.active).map(|window| window.id) else {
                        continue;
                    };
                    if old != new && self.window_file_id(old).is_some() {
                        self.swap_windows(old, new)?;
                        plan.operations.push_front(Operation::FocusWindow(new));
                        plan.operations.push_front(Operation::Close {
                            window: old,
                            force: true,
                        });
                    }
                }
                Operation::FocusWindow(window) => {
                    if self.window_file_id(window).is_some() {
                        self.focus_window(window)?;
                    }
                }
                Operation::ChangeDirectory(path) => {
                    let path = path
                        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
                        .ok_or_else(|| invalid("HOME is not set"))?;
                    let path = std::fs::canonicalize(self.resolve_path(&path))?;
                    if !path.is_dir() {
                        return Err(invalid("not a directory"));
                    }
                    self.directory = path;
                    self.editor.set_directory(&self.directory);
                }
                Operation::Close { window, force } => {
                    let Some(index) = self.windows.iter().position(|w| w.id == window) else {
                        continue;
                    };
                    if self.closing.contains(&window) {
                        continue;
                    }
                    if !force && !self.closable(index) {
                        return Err(invalid("unsaved changes; use :w or :q!"));
                    }
                    self.closing.insert(window);
                    plan.closing.push(window);
                    plan.operations.push_front(Operation::CloseFile { window });
                    return Ok(HostStep::WindowClose(window));
                }
                Operation::CloseFile { window } => {
                    let Some(w) = self.windows.iter().find(|w| w.id == window) else {
                        self.closing.remove(&window);
                        continue;
                    };
                    let buffer = w.buffer;
                    plan.operations
                        .push_front(Operation::RemoveWindow { window });
                    if !self.buffers[buffer].as_ref().is_some_and(|b| b.internal)
                        && self.windows.iter().filter(|w| w.buffer == buffer).count() == 1
                    {
                        return Ok(HostStep::FileClose(buffer));
                    }
                }
                Operation::RemoveWindow { window } => {
                    self.closing.remove(&window);
                    let Some(index) = self.windows.iter().position(|w| w.id == window) else {
                        continue;
                    };
                    self.remove_window(index)?;
                    if self.external_window_ids().is_empty() {
                        plan.outcome = Outcome::Quit;
                    }
                }
                Operation::Save(save) => {
                    let public = save.public.clone();
                    plan.operations.push_front(Operation::CommitSave(save));
                    return Ok(HostStep::SavePre(public));
                }
                Operation::CommitSave(save) => {
                    if !allow && !save.public.force {
                        return Err(invalid("write cancelled by FILE_SAVE_PRE"));
                    }
                    let p = &save.public;
                    if self.with_buffer(p.buffer, |_| ()).is_none() {
                        return Err(invalid("save target was closed during FILE_SAVE_PRE"));
                    }
                    let ranges = if save.whole {
                        None
                    } else {
                        p.ranges.as_deref()
                    };
                    let written = if save.stdout {
                        use std::io::Write;
                        let snapshot = self
                            .with_buffer(p.buffer, |t| {
                                if let Some(r) = ranges {
                                    t.read_ranges(r)
                                } else {
                                    Ok(t.read_snapshot())
                                }
                            })
                            .expect("live buffer")
                            .map_err(|e| invalid(e.to_string()))?;
                        let mut out = io::stdout().lock();
                        for chunk in snapshot.chunks() {
                            out.write_all(chunk)?;
                        }
                        out.flush()?;
                        self.with_buffer_mut(p.buffer, Text::mark_saved);
                        snapshot.len()
                    } else {
                        self.save_buffer_ranges(
                            p.buffer,
                            save.explicit.then_some(p.path.as_path()),
                            p.force,
                            ranges,
                            save.visual,
                        )?
                    };
                    plan.outcome = Outcome::Message(format!("{written} bytes written"));
                    return Ok(HostStep::SavePost {
                        buffer: p.buffer,
                        path: p.path.clone(),
                    });
                }
                Operation::Native(EditorAction::Input(request)) => {
                    plan.inputs.insert(request.token);
                    return Ok(HostStep::Input(request));
                }
                Operation::Native(EditorAction::Callback(request)) => {
                    plan.callbacks.insert(request.token);
                    if let Some(window) =
                        (!matches!(request.kind, crate::editor::CallbackKind::Mapping { .. }))
                            .then_some(
                                request
                                    .window
                                    .or_else(|| self.windows.get(self.active).map(|w| w.id as u64)),
                            )
                            .flatten()
                    {
                        plan.callback_windows.insert(request.token, window as usize);
                    }
                    return Ok(HostStep::Callback(request));
                }
                Operation::Native(action) => {
                    plan.outcome = self.apply_native(action)?;
                }
                Operation::FinishError(message) => return Err(invalid(message)),
            }
        }
    }
    pub(super) fn remove_window(&mut self, index: usize) -> io::Result<()> {
        let parent = self.windows[index].prompt.as_ref().and_then(|p| p.parent);
        self.editor
            .forget_window_mappings(self.windows[index].id as u64);
        if self.message_window == Some(self.windows[index].id) {
            self.message_window = None;
        }
        if index == self.active {
            self.capture();
            let buffer = self.windows[index].buffer;
            let old = self.buffers[buffer].as_mut().expect("live buffer");
            std::mem::swap(&mut self.editor.text, &mut old.text);
            self.editor.swap_buffer_state(&mut old.state);
            self.windows.remove(index);
            if !self.windows.is_empty() {
                self.active = parent
                    .and_then(|id| self.windows.iter().position(|w| w.id == id))
                    .unwrap_or_else(|| index.min(self.windows.len() - 1));
                self.install_view_raw()?;
            } else {
                self.active = 0;
            }
        } else {
            self.windows.remove(index);
            if index < self.active {
                self.active -= 1;
            }
        }
        self.collect_closed_buffers();
        Ok(())
    }
    /// Native `sam_cmd` epilogue: a completed `:`-command records the focused
    /// window's selection after handlers such as `:split` may have moved focus.
    pub(crate) fn finish_command(&mut self) {
        if !self.windows.is_empty() {
            self.editor.jumplist_save();
        }
    }
    pub(super) fn drive_without_events(&mut self, mut plan: HostPlan) -> io::Result<Outcome> {
        loop {
            match self.advance(&mut plan, true) {
                Ok(HostStep::Complete(outcome)) => return Ok(outcome),
                Ok(HostStep::Input(request)) => {
                    self.resume_input(&mut plan, request.token, false)?;
                }
                Ok(HostStep::Callback(_)) => return Err(invalid("callback requires a Lua host")),
                Ok(HostStep::Command(_)) => return Err(invalid("command requires a Lua host")),
                Ok(HostStep::TargetCommand { token, request }) => {
                    let accepted = match self.execute_target_command(&request) {
                        Ok(EditorAction::Continue) => true,
                        Ok(action) => {
                            let child = self.begin_target_action(request.window, action)?;
                            self.drive_without_events(child).is_ok()
                        }
                        Err(error) => {
                            if let Some(execution) = plan.structural.as_mut() {
                                execution.host_error(&error);
                            }
                            plan.outcome = Outcome::Message(error.to_string());
                            false
                        }
                    };
                    self.resume_target_command(&mut plan, token, accepted)?;
                }
                Ok(HostStep::PromptCommand { token, command }) => {
                    let accepted = match self.prepare_command(&command) {
                        Ok(next) => {
                            plan.outcome = self
                                .drive_without_events(next)
                                .unwrap_or_else(|error| Outcome::Message(error.to_string()));
                            self.finish_command();
                            true
                        }
                        Err(error) => {
                            plan.outcome = Outcome::Message(error.to_string());
                            false
                        }
                    };
                    self.resume_prompt_command(&mut plan, token, accepted)?;
                }
                Ok(_) => {}
                Err(error) => {
                    self.cancel_plan(&plan);
                    return Err(error);
                }
            }
        }
    }
}

impl Workspace {
    /// Resume a submission after the host has run the command synchronously,
    /// possibly through reentrant Lua and nested lifecycle plans.
    pub fn resume_prompt_command(
        &mut self,
        plan: &mut HostPlan,
        token: u64,
        success: bool,
    ) -> io::Result<()> {
        let submission = plan
            .prompts
            .remove(&token)
            .ok_or_else(|| invalid("unknown or already completed prompt token"))?;
        plan.operations.push_front(Operation::FinishPrompt {
            submission,
            success,
        });
        Ok(())
    }

    pub fn apply(&mut self, action: EditorAction) -> io::Result<Outcome> {
        let plan = self.begin_action(action)?;
        self.drive_without_events(plan)
    }
    pub fn open_scratch(&mut self, input: Option<Vec<u8>>, read_only: bool) -> io::Result<()> {
        let plan = self.begin_scratch(input, read_only)?;
        self.drive_without_events(plan).map(|_| ())
    }
    pub fn file_ids(&self) -> Vec<usize> {
        self.buffers
            .iter()
            .enumerate()
            .filter_map(|(id, b)| b.as_ref().filter(|b| !b.internal).map(|_| id))
            .collect()
    }
    pub fn all_file_ids(&self) -> Vec<usize> {
        self.buffers
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(id, buffer)| buffer.as_ref().map(|_| id))
            .collect()
    }
    pub fn window_ids(&self) -> Vec<usize> {
        self.windows.iter().map(|w| w.id).collect()
    }
    pub fn window_file_id(&self, id: usize) -> Option<usize> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.buffer)
    }
    pub fn with_buffer<R>(&self, id: usize, f: impl FnOnce(&Text) -> R) -> Option<R> {
        self.buffers.get(id)?.as_ref()?;
        Some(f(self.text(id)))
    }
    pub fn with_buffer_mut<R>(&mut self, id: usize, f: impl FnOnce(&mut Text) -> R) -> Option<R> {
        self.buffers.get(id)?.as_ref()?;
        if self
            .windows
            .get(self.active)
            .is_some_and(|w| w.buffer == id)
        {
            Some(self.editor.mutate_text(f))
        } else {
            Some(f(&mut self.buffers[id].as_mut()?.text))
        }
    }
    pub fn buffer_path(&self, id: usize) -> Option<PathBuf> {
        self.buffers
            .get(id)?
            .as_ref()?
            .document
            .as_ref()
            .map(|d| d.path().to_owned())
    }
    pub fn buffer_permission(&self, id: usize) -> Option<u32> {
        self.buffers.get(id)?.as_ref()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            Some(
                self.buffer_path(id)
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map_or(0o666, |m| m.permissions().mode() & 0o7777),
            )
        }
        #[cfg(not(unix))]
        {
            Some(if self.buffers[id].as_ref()?.read_only {
                0o444
            } else {
                0o666
            })
        }
    }
    pub fn buffer_save_method(&self, id: usize) -> Option<SaveMethod> {
        Some(self.buffers.get(id)?.as_ref()?.save_method)
    }
    pub fn set_buffer_save_method(&mut self, id: usize, method: SaveMethod) -> io::Result<()> {
        self.buffers
            .get_mut(id)
            .and_then(Option::as_mut)
            .ok_or_else(|| invalid("closed file"))?
            .save_method = method;
        Ok(())
    }
    pub fn focus_window(&mut self, id: usize) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        self.focus(index)
    }
    /// Swap native list positions and focus the counterpart when either was
    /// active. Window, file and selection identities are not exchanged.
    pub fn swap_windows(&mut self, first: usize, second: usize) -> io::Result<()> {
        let a = self
            .windows
            .iter()
            .position(|window| window.id == first)
            .ok_or_else(|| invalid("closed window"))?;
        let b = self
            .windows
            .iter()
            .position(|window| window.id == second)
            .ok_or_else(|| invalid("closed window"))?;
        if a == b {
            return Ok(());
        }
        let active = self.active_window_id();
        self.windows.swap(a, b);
        self.active = self
            .windows
            .iter()
            .position(|window| window.id == active)
            .expect("active window");
        let target = if active == first {
            second
        } else if active == second {
            first
        } else {
            active
        };
        self.focus_window(target)
    }
    pub fn window_identified_selections(&self, id: usize) -> Option<Vec<(SelectionId, Selection)>> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        Some(if index == self.active {
            self.editor.view_identified()
        } else {
            w.view
                .selections
                .iter()
                .map(|s| s.id)
                .zip(
                    w.view
                        .resolve(self.text(w.buffer), &self.text(w.buffer).to_vec()),
                )
                .collect()
        })
    }
    pub fn window_primary_selection(&self, id: usize) -> Option<SelectionId> {
        let (i, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        if i == self.active {
            self.editor
                .selection_ids()
                .get(self.editor.primary_index())
                .copied()
        } else {
            w.view.selections.get(w.view.primary).map(|s| s.id)
        }
    }
    pub fn window_update_selection(
        &mut self,
        id: usize,
        selection: SelectionId,
        value: Selection,
    ) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            return self
                .editor
                .update_selection(selection, value)
                .map_err(|e| invalid(e.to_string()));
        }
        let buffer = self.windows[index].buffer;
        let marks = value
            .save(self.text(buffer))
            .map_err(|e| invalid(e.to_string()))?;
        let stored = self.windows[index]
            .view
            .selections
            .iter_mut()
            .find(|s| s.id == selection)
            .ok_or_else(|| invalid("removed selection"))?;
        stored.mark = Some(marks);
        stored.fallback = value;
        Ok(())
    }
    pub fn window_remove_selection(&mut self, id: usize, selection: SelectionId) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            return self
                .editor
                .remove_selection(selection)
                .map(|_| ())
                .map_err(|e| invalid(e.to_string()));
        }
        let view = &mut self.windows[index].view;
        let position = view
            .selections
            .iter()
            .position(|s| s.id == selection)
            .ok_or_else(|| invalid("removed selection"))?;
        if view.selections.len() > 1 {
            view.selections.remove(position);
            if position < view.primary {
                view.primary -= 1;
            }
            view.primary = view.primary.min(view.selections.len() - 1);
        }
        Ok(())
    }
    pub fn window_set_primary_selection(
        &mut self,
        id: usize,
        selection: SelectionId,
    ) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            return self
                .editor
                .set_primary_selection(selection)
                .map_err(|e| invalid(e.to_string()));
        }
        let view = &mut self.windows[index].view;
        view.primary = view
            .selections
            .iter()
            .position(|s| s.id == selection)
            .ok_or_else(|| invalid("removed selection"))?;
        Ok(())
    }
    pub fn window_selection_anchored(&self, id: usize, selection: SelectionId) -> Option<bool> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        if index == self.active {
            self.editor.selection_anchored(selection)
        } else {
            w.view
                .selections
                .iter()
                .find(|s| s.id == selection)
                .map(|s| s.anchored)
        }
    }
    pub fn set_window_selection_anchored(
        &mut self,
        id: usize,
        selection: SelectionId,
        value: bool,
    ) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            return self
                .editor
                .set_selection_anchored(selection, value)
                .map_err(|e| invalid(e.to_string()));
        }
        let stored = self.windows[index]
            .view
            .selections
            .iter_mut()
            .find(|s| s.id == selection)
            .ok_or_else(|| invalid("removed selection"))?;
        stored.anchored = value;
        Ok(())
    }
    pub fn window_selection_position(&self, id: usize, selection: SelectionId) -> Option<usize> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        if index == self.active {
            self.editor.selection_position(selection)
        } else {
            let stored = w.view.selections.iter().find(|s| s.id == selection)?;
            if let Some(mark) = &stored.mark {
                mark.restore(self.text(w.buffer)).map(|s| s.cursor)
            } else {
                Some(stored.fallback.cursor)
            }
        }
    }
    pub fn window_options(&self, id: usize) -> Option<(usize, bool, bool)> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        Some(if index == self.active {
            (
                self.editor.tab_width,
                self.editor.expand_tab,
                self.editor.auto_indent,
            )
        } else {
            (w.view.tab_width, w.view.expand_tab, self.editor.auto_indent)
        })
    }
    pub fn set_window_options(
        &mut self,
        id: usize,
        tab_width: usize,
        expand_tab: bool,
        auto_indent: bool,
    ) -> io::Result<()> {
        if !(1..=8).contains(&tab_width) {
            return Err(invalid("tab width must be 1..8"));
        }
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        let view = &mut self.windows[index].view;
        view.tab_width = tab_width;
        view.expand_tab = expand_tab;
        self.editor.auto_indent = auto_indent;
        if index == self.active {
            self.editor.tab_width = tab_width;
            self.editor.expand_tab = expand_tab;
        }
        Ok(())
    }
}

impl Workspace {
    /// Record the UI-measured body geometry used by viewport motions. Unknown
    /// windows are ignored because a draw may finish after its window closed.
    pub fn set_window_viewport(&mut self, id: usize, mut viewport: crate::view::ViewportMetrics) {
        let Some(index) = self.windows.iter().position(|window| window.id == id) else {
            return;
        };
        viewport.wrapcolumn = self.windows[index].display.wrapcolumn;
        self.windows[index].view.viewport = Some(viewport);
        if index == self.active {
            self.editor.set_viewport(Some(viewport));
            // Native `view->start_mark` keeps the visible content in place
            // when the text changes above the view.
            self.editor.set_view_anchor(viewport.top);
        }
    }
    pub fn window_selection_range(
        &self,
        id: usize,
        selection: SelectionId,
    ) -> Option<std::ops::Range<usize>> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        if index == self.active {
            return self.editor.selection_range(selection);
        }
        let saved = w.view.selections.iter().find(|s| s.id == selection)?;
        let text = self.text(w.buffer);
        let selection = if let Some(mark) = &saved.mark {
            mark.restore(text)?
        } else {
            saved.fallback
        };
        selection.range(&text.to_vec()).ok()
    }
    pub fn window_mark_get(&self, id: usize, name: char) -> Option<Vec<std::ops::Range<usize>>> {
        let (index, w) = self.windows.iter().enumerate().find(|(_, w)| w.id == id)?;
        if name == '^' && index != self.active {
            let text = self.text(w.buffer);
            let data = text.to_vec();
            return Some(
                w.view
                    .last_visual
                    .iter()
                    .filter_map(|m| m.restore(text)?.range(&data).ok())
                    .collect(),
            );
        }
        Some(
            if index == self.active
                || self
                    .windows
                    .get(self.active)
                    .is_some_and(|active| active.buffer == w.buffer)
            {
                self.editor.named_mark_get(name)
            } else {
                self.buffers[w.buffer]
                    .as_ref()?
                    .state
                    .marked_ranges(self.text(w.buffer), name)
            },
        )
    }
    pub fn window_mark_set(
        &mut self,
        id: usize,
        name: char,
        ranges: Vec<std::ops::Range<usize>>,
    ) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|w| w.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            return self
                .editor
                .named_mark_set(name, ranges)
                .map_err(|e| invalid(e.to_string()));
        }
        let buffer = self.windows[index].buffer;
        if name == '^' {
            let text = self.text(buffer);
            let data = text.to_vec();
            let marks = ranges
                .into_iter()
                .map(|r| {
                    Selection::from_range(&data, r, crate::selection::Direction::Forward)
                        .and_then(|s| s.save(text))
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| invalid(e.to_string()))?;
            self.windows[index].view.last_visual = marks;
            return Ok(());
        }
        if self
            .windows
            .get(self.active)
            .is_some_and(|w| w.buffer == buffer)
        {
            return self
                .editor
                .named_mark_set(name, ranges)
                .map_err(|e| invalid(e.to_string()));
        }
        let target = self.buffers[buffer]
            .as_mut()
            .ok_or_else(|| invalid("closed file"))?;
        target
            .state
            .set_marked_ranges(&target.text, name, ranges)
            .map_err(|e| invalid(e.to_string()))
    }
}

impl Workspace {
    /// Internal prompt/message buffers are owned normally but omitted from Lua's
    /// public iterators and lifecycle notifications, as in the original host.
    pub fn buffer_internal(&self, id: usize) -> Option<bool> {
        Some(self.buffers.get(id)?.as_ref()?.internal)
    }
    pub fn window_internal(&self, id: usize) -> Option<bool> {
        self.buffer_internal(self.window_file_id(id)?)
    }
    pub fn external_window_ids(&self) -> Vec<usize> {
        self.windows
            .iter()
            .filter(|w| !self.buffers[w.buffer].as_ref().is_some_and(|b| b.internal))
            .map(|w| w.id)
            .collect()
    }
}

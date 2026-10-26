//! Borrow-free workspace adaptation of the typed structural command machine.
use super::*;
use crate::sam::{
    CommittedChange, HostRequest, SamError, SamStep, SelectionEffect, SelectionInput, WindowContext,
};

pub(super) struct Execution {
    pub token: u64,
    pub continuation: crate::sam::Continuation,
    pub waiting: bool,
    pub message: Option<String>,
    compatible: bool,
    host_failure: Option<String>,
    host_failed: bool,
    structural_failure: Option<SamError>,
    disposed: BTreeSet<usize>,
    initial_position: usize,
}

impl Execution {
    pub fn new(
        token: u64,
        program: crate::sam::Program,
        window: usize,
        compatible: bool,
        position: usize,
    ) -> Self {
        Self {
            token,
            continuation: program.start(window),
            waiting: false,
            message: None,
            compatible,
            host_failure: None,
            host_failed: false,
            structural_failure: None,
            disposed: BTreeSet::new(),
            initial_position: position,
        }
    }
    pub fn host_error(&mut self, error: &io::Error) {
        self.host_failed = true;
        self.host_failure.get_or_insert_with(|| error.to_string());
        self.message = Some(error.to_string());
    }
    pub fn host_result(&mut self, success: bool) {
        self.host_failed |= !success;
    }
}

impl Workspace {
    /// Execute an already parsed non-lifecycle handler against its explicit
    /// window. This never focuses the target or reparses a command string.
    pub fn execute_target_command(&mut self, request: &HostRequest) -> io::Result<EditorAction> {
        use crate::sam::HostCommand as H;
        if self.window_file_id(request.window) != Some(request.file) {
            return Err(invalid("structural target was closed or reloaded"));
        }
        match request.command {
            H::Map | H::Unmap | H::MapWindow | H::UnmapWindow | H::LangMap => self
                .editor
                .execute_sam_host(request)
                .map_err(|error| invalid(error.to_string())),
            H::Earlier | H::Later => {
                self.with_buffer_mut(request.file, |text| {
                    crate::history::execute_words(
                        text,
                        request.command == H::Earlier,
                        &request.args,
                    )
                })
                .ok_or_else(|| invalid("closed file"))?
                .map_err(|error| invalid(error.to_string()))?;
                Ok(EditorAction::Continue)
            }
            H::Set => {
                if request.args.is_empty() || request.args.len() > 2 {
                    return Err(invalid("expected set option [value]"));
                }
                let (raw, equal_value) = request.args[0]
                    .split_once('=')
                    .map_or((request.args[0].as_str(), None), |(name, value)| {
                        (name, Some(value))
                    });
                let toggle = raw.ends_with('!');
                let raw = raw.trim_end_matches('!');
                let (name, negate) = raw
                    .strip_prefix("no")
                    .map_or((raw, false), |name| (name, true));
                let value = equal_value.or_else(|| request.args.get(1).map(String::as_str));
                self.apply_set_option(name, value, toggle, negate, request.window)?;
                Ok(EditorAction::Continue)
            }
            H::Substitute => Err(invalid(
                "substitution is unsupported; use x/pattern/ c/replacement/",
            )),
            H::User(_) => Err(invalid("registered command requires a Lua host")),
            _ => Err(invalid("lifecycle handler requires a host plan")),
        }
    }

    fn structural_inputs(&self, window: usize) -> Vec<SelectionInput> {
        let visual = matches!(self.editor.mode, Mode::Visual | Mode::VisualLine);
        self.window_identified_selections(window)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, _)| {
                let range = if visual {
                    self.window_selection_range(window, id).unwrap_or(0..0)
                } else {
                    let position = self.window_selection_position(window, id).unwrap_or(0);
                    position..position
                };
                SelectionInput {
                    id: id.get(),
                    range,
                }
            })
            .collect()
    }

    /// Apply a native `set option [value]` command.
    pub(super) fn apply_set_option(
        &mut self,
        name: &str,
        value: Option<&str>,
        toggle: bool,
        negate: bool,
        window: usize,
    ) -> io::Result<()> {
        let (mut width, mut expand, mut auto) = self
            .window_options(window)
            .ok_or_else(|| invalid("closed window"))?;
        let boolean = |current: bool| -> io::Result<bool> {
            if toggle {
                if value.is_some() {
                    return Err(invalid("toggle does not accept a value"));
                }
                return Ok(!current);
            }
            if negate {
                return Ok(false);
            }
            match value {
                None | Some("1" | "true" | "yes" | "on") => Ok(true),
                Some("0" | "false" | "no" | "off") => Ok(false),
                _ => Err(invalid("expected boolean option value")),
            }
        };
        match name {
            "tabwidth" | "tw" => {
                width = value
                    .and_then(|value| value.parse::<usize>().ok())
                    .filter(|value| (1..=8).contains(value))
                    .ok_or_else(|| invalid("tab width must be 1..8"))?;
            }
            "expandtab" | "et" => expand = boolean(expand)?,
            "autoindent" | "ai" => auto = boolean(auto)?,
            "shell" => {
                let shell = value.ok_or_else(|| invalid("shell option requires a program"))?;
                self.editor.set_shell(Some(shell.to_string()));
            }
            "escdelay" => {
                let delay = value
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or_else(|| invalid("escape delay must be a number"))?;
                self.editor
                    .set_escape_delay(Some(std::time::Duration::from_millis(delay)));
            }
            "statusbar" | "sb" | "showeof" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                if name == "showeof" {
                    display.showeof = boolean(display.showeof)?;
                } else {
                    display.statusbar = boolean(display.statusbar)?;
                }
                self.set_window_display(window, display)?;
            }
            "numbers" | "nu" | "relativenumbers" | "rnu" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                // The native absolute and relative number options are
                // mutually exclusive.
                let relative = matches!(name, "relativenumbers" | "rnu");
                let enabled = boolean(if relative {
                    display.relativenumbers
                } else {
                    display.numbers
                })?;
                display.numbers = enabled && !relative;
                display.relativenumbers = enabled && relative;
                self.set_window_display(window, display)?;
            }
            "numberwidth" | "nuw" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                display.numberwidth = value
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| invalid("number width must be a number"))?;
                self.set_window_display(window, display)?;
            }
            "cursorline" | "cul" | "showspaces" | "showtabs" | "shownewlines" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                let current = match name {
                    "cursorline" | "cul" => display.cursorline,
                    "showspaces" => display.showspaces,
                    "showtabs" => display.showtabs,
                    _ => display.shownewlines,
                };
                let enabled = boolean(current)?;
                match name {
                    "cursorline" | "cul" => display.cursorline = enabled,
                    "showspaces" => display.showspaces = enabled,
                    "showtabs" => display.showtabs = enabled,
                    _ => display.shownewlines = enabled,
                }
                self.set_window_display(window, display)?;
            }
            "colorcolumn" | "cc" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                display.colorcolumn = value
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| invalid("color column must be a number"))?;
                self.set_window_display(window, display)?;
            }
            "wrapcolumn" | "wc" => {
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                display.wrapcolumn = value
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| invalid("wrap column must be a number"))?;
                self.set_window_display(window, display)?;
            }
            "breakat" | "brk" => {
                let characters = value.ok_or_else(|| invalid("break characters are required"))?;
                let mut display = self
                    .window_display(window)
                    .ok_or_else(|| invalid("closed window"))?;
                display.breakat = std::sync::Arc::from(characters);
                self.set_window_display(window, display)?;
                self.editor.set_breakat(Some(characters.to_string()));
            }
            _ => return Err(invalid(format!("unsupported option: {name}"))),
        }
        self.set_window_options(window, width, expand, auto)?;
        Ok(())
    }

    fn structural_step(&mut self, execution: &mut Execution) -> Result<SamStep, SamError> {
        // Control frames can unwind after their original window was closed.
        // Ask the machine which live context it actually needs before looking
        // up any window; a frame's associated ID alone is not such a request.
        let step = execution
            .continuation
            .advance(self.editor.sam_session_mut(), None)?;
        let SamStep::NeedWindow(window) = step else {
            return Ok(step);
        };
        let Some(file) = self.window_file_id(window) else {
            execution.continuation.skip_window(window)?;
            return execution
                .continuation
                .advance(self.editor.sam_session_mut(), None);
        };
        let selections = self.structural_inputs(window);
        let visual = matches!(self.editor.mode, Mode::Visual | Mode::VisualLine);
        let active_file = self.windows.get(self.active).map(|window| window.buffer);
        if active_file == Some(file) {
            self.editor.with_sam_text_session(|text, session| {
                execution.continuation.advance(
                    session,
                    Some(WindowContext {
                        window,
                        file,
                        text,
                        selections: &selections,
                        visual,
                    }),
                )
            })
        } else {
            let text = &self.buffers[file].as_ref().expect("live target file").text;
            execution.continuation.advance(
                self.editor.sam_session_mut(),
                Some(WindowContext {
                    window,
                    file,
                    text,
                    selections: &selections,
                    visual,
                }),
            )
        }
    }

    pub(super) fn advance_structural(
        &mut self,
        execution: &mut Execution,
    ) -> io::Result<Option<HostRequest>> {
        loop {
            match self.structural_step(execution).map_err(io::Error::other)? {
                SamStep::NeedWindow(window) => {
                    if self.window_file_id(window).is_none() {
                        execution
                            .continuation
                            .skip_window(window)
                            .map_err(io::Error::other)?;
                    }
                }
                SamStep::Files(request) => {
                    let mut targets = Vec::new();
                    for window in self.external_window_ids() {
                        let path = self
                            .window_file_id(window)
                            .and_then(|file| self.buffer_path(file));
                        let bytes = path
                            .as_ref()
                            .map_or(&[][..], |path| path.as_os_str().as_encoded_bytes());
                        if request.matches(bytes).map_err(io::Error::other)? {
                            targets.push(window);
                        }
                    }
                    execution
                        .continuation
                        .resume_files(targets)
                        .map_err(io::Error::other)?;
                }
                SamStep::Host(request) => {
                    if self.window_file_id(request.window) != Some(request.file) {
                        execution
                            .continuation
                            .resume_host(false)
                            .map_err(io::Error::other)?;
                    } else {
                        return Ok(Some(request));
                    }
                }
                SamStep::Selection(request) => {
                    if self.window_file_id(request.window).is_some() {
                        match request.effect {
                            SelectionEffect::Print { source, range } => {
                                if self.window_file_id(request.window) == Some(request.file) {
                                    self.structural_print(
                                        execution,
                                        request.window,
                                        source,
                                        range,
                                    )?;
                                }
                            }
                            SelectionEffect::Dispose { source } => {
                                self.structural_dispose(execution, request.window, source)?
                            }
                        }
                    }
                    execution
                        .continuation
                        .resume_selection()
                        .map_err(io::Error::other)?;
                }
                SamStep::Diagnostic(error) => {
                    execution.message = Some(error.to_string());
                    execution.structural_failure.get_or_insert(error);
                }
                SamStep::Complete => {
                    if let Some(error) = execution.structural_failure.take() {
                        if !execution.compatible {
                            return Err(io::Error::other(error));
                        }
                        execution.message = Some(error.to_string());
                        return Ok(None);
                    }
                    if !execution.compatible && execution.host_failed {
                        return Err(invalid(
                            execution
                                .host_failure
                                .take()
                                .unwrap_or_else(|| "command handler failed".into()),
                        ));
                    }
                    self.commit_structural(execution)?;
                    return Ok(None);
                }
            }
        }
    }

    fn structural_id(&self, window: usize, raw: u64) -> Option<SelectionId> {
        self.window_identified_selections(window)?
            .into_iter()
            .find(|(id, _)| id.get() == raw)
            .map(|(id, _)| id)
    }

    fn structural_print(
        &mut self,
        execution: &mut Execution,
        window: usize,
        source: Option<u64>,
        range: std::ops::Range<usize>,
    ) -> io::Result<()> {
        let file = self
            .window_file_id(window)
            .ok_or_else(|| invalid("closed structural window"))?;
        let data = self.text(file).to_vec();
        let selection =
            Selection::from_range(&data, range.clone(), crate::selection::Direction::Forward)
                .map_err(|error| invalid(error.to_string()))?;
        if let Some(id) = source.and_then(|raw| self.structural_id(window, raw)) {
            self.window_update_selection(window, id, selection)?;
            self.set_window_selection_anchored(window, id, !range.is_empty())?;
            return Ok(());
        }
        let index = self
            .windows
            .iter()
            .position(|candidate| candidate.id == window)
            .ok_or_else(|| invalid("closed window"))?;
        if index == self.active {
            self.capture();
        }
        let id = SelectionId::fresh().map_err(|error| invalid(error.to_string()))?;
        let mark = selection.save(self.text(file)).ok();
        let view = &mut self.windows[index].view;
        let dead = execution.disposed.remove(&window);
        if dead {
            view.selections.clear();
            view.primary = 0;
        }
        let position = view
            .selections
            .iter()
            .position(|saved| saved.fallback.cursor > range.start)
            .unwrap_or(view.selections.len());
        if !dead && position <= view.primary {
            view.primary += 1;
        }
        view.selections.insert(
            position,
            StoredSelection {
                id,
                fallback: selection,
                mark,
                anchored: !range.is_empty(),
            },
        );
        if index == self.active {
            self.install_active_selection_view()?;
        }
        Ok(())
    }

    fn structural_dispose(
        &mut self,
        execution: &mut Execution,
        window: usize,
        raw: u64,
    ) -> io::Result<()> {
        let Some(id) = self.structural_id(window, raw) else {
            return Ok(());
        };
        if self
            .window_identified_selections(window)
            .is_some_and(|selections| selections.len() == 1)
        {
            execution.disposed.insert(window);
            self.set_window_selection_anchored(window, id, false)?;
        } else {
            self.window_remove_selection(window, id)?;
        }
        Ok(())
    }

    fn install_active_selection_view(&mut self) -> io::Result<()> {
        let view = &self.windows[self.active].view;
        let selections = view
            .selections
            .iter()
            .zip(view.resolve(&self.editor.text, &self.editor.text.to_vec()))
            .map(|(saved, selection)| (saved.id, selection, saved.anchored))
            .collect();
        self.editor
            .install_view_state_raw(selections, view.selections[view.primary].id)
            .map_err(|error| invalid(error.to_string()))?;
        self.editor
            .restore_selection_marks(
                view.selections
                    .iter()
                    .filter_map(|saved| saved.mark.clone().map(|mark| (saved.id, mark)))
                    .collect(),
            )
            .map_err(|error| invalid(error.to_string()))
    }

    fn commit_structural(&mut self, execution: &mut Execution) -> io::Result<()> {
        let files = execution.continuation.pending_files();
        for file in &files {
            if self.with_buffer(*file, |_| ()).is_none() {
                execution.continuation.discard_file(*file);
                continue;
            }
            self.with_buffer(*file, |text| {
                execution.continuation.validate_file(*file, text)
            })
            .expect("live file")
            .map_err(io::Error::other)?;
        }
        for file in files {
            let Some(changes) =
                self.with_buffer_mut(file, |text| execution.continuation.commit_file(file, text))
            else {
                continue;
            };
            for change in changes.map_err(io::Error::other)? {
                self.apply_committed_change(execution, file, change)?;
            }
        }
        let windows = self.window_ids();
        for window in windows {
            let index = self
                .windows
                .iter()
                .position(|candidate| candidate.id == window)
                .expect("live window");
            if index == self.active {
                self.capture();
            }
            let text = self.text(self.windows[index].buffer);
            let data = text.to_vec();
            let view = &self.windows[index].view;
            let mut set = SelectionSet::from_identified(
                &data,
                view.selections
                    .iter()
                    .map(|selection| selection.id)
                    .zip(view.resolve(text, &data))
                    .collect(),
                view.selections[view.primary].id,
            )
            .map_err(|error| invalid(error.to_string()))?;
            set.normalize_native(&data)
                .map_err(|error| invalid(error.to_string()))?;
            let stored = set
                .identified()
                .into_iter()
                .map(|(id, selection)| StoredSelection {
                    id,
                    fallback: selection,
                    mark: selection.save(text).ok(),
                    anchored: view
                        .selections
                        .iter()
                        .find(|saved| saved.id == id)
                        .is_some_and(|saved| saved.anchored),
                })
                .collect();
            let primary = set.primary_index();
            self.windows[index].view.selections = stored;
            self.windows[index].view.primary = primary;
            if index == self.active {
                self.install_active_selection_view()?;
            }
        }
        if let Some(window) = self.windows.get(self.active).map(|window| window.id) {
            if execution.disposed.contains(&window) {
                let primary = self
                    .window_primary_selection(window)
                    .expect("primary selection");
                let position = execution.initial_position.min(self.editor.text.len());
                self.window_update_selection(
                    window,
                    primary,
                    Selection {
                        anchor: position,
                        cursor: position,
                    },
                )?;
            }
            if let Some((primary, _)) = self
                .window_identified_selections(window)
                .and_then(|selections| selections.into_iter().next())
            {
                self.window_set_primary_selection(window, primary)?;
            }
            let anchored = self
                .editor
                .selection_ids()
                .iter()
                .any(|id| self.editor.selection_anchored(*id).unwrap_or(false));
            self.editor
                .switch_mode(if anchored { Mode::Visual } else { Mode::Normal })
                .map_err(|error| invalid(error.to_string()))?;
        }
        Ok(())
    }

    fn apply_committed_change(
        &mut self,
        execution: &mut Execution,
        file: usize,
        committed: CommittedChange,
    ) -> io::Result<()> {
        let window = committed.window;
        if self.window_file_id(window) != Some(file) {
            return Ok(());
        }
        let change = committed.change;
        if let Some(id) = committed
            .selection
            .and_then(|raw| self.structural_id(window, raw))
        {
            if !change.insertion && execution.continuation.visual() {
                self.structural_dispose(execution, window, id.get())?;
            } else if change.insertion && execution.continuation.visual() {
                self.structural_print(execution, window, Some(id.get()), change.range)?;
            } else {
                let position = if change.contains_newline || !change.insertion {
                    change.range.start
                } else {
                    change.range.end
                }
                .min(self.text(file).len());
                let anchor = if self.window_selection_anchored(window, id) == Some(true) {
                    self.window_identified_selections(window)
                        .and_then(|selections| {
                            selections
                                .into_iter()
                                .find(|(candidate, _)| *candidate == id)
                        })
                        .map_or(position, |(_, selection)| selection.anchor)
                } else {
                    position
                };
                self.window_update_selection(
                    window,
                    id,
                    Selection {
                        anchor,
                        cursor: position,
                    },
                )?;
            }
        } else if change.insertion && execution.continuation.visual() {
            self.structural_print(execution, window, None, change.range)?;
        }
        Ok(())
    }
}

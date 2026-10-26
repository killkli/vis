//! Real internal prompt windows share one persistent, editable history buffer.
use super::*;
use crate::text::Mark;

pub(super) struct PromptSession {
    pub(super) parent: Option<usize>,
    parent_mode: Mode,
    action: Option<PromptActionState>,
}

#[derive(Clone)]
pub(super) struct Submission {
    pub token: u64,
    pub window: usize,
    buffer: usize,
    range: Option<(Mark, Mark)>,
    pub command: Vec<u8>,
    lastline: bool,
}

impl Workspace {
    /// Unlike normal focus, prompt transitions preserve the global input engine
    /// and install only the target view after moving its Text into the editor.
    pub(super) fn focus_raw(&mut self, index: usize) -> io::Result<()> {
        if index >= self.windows.len() {
            return Err(invalid("no such window"));
        }
        if index == self.active {
            return Ok(());
        }
        self.windows[index]
            .view
            .validate(self.text(self.windows[index].buffer))?;
        self.capture();
        let old = self.windows[self.active].buffer;
        let previous = self.buffers[old].as_mut().expect("live buffer");
        std::mem::swap(&mut self.editor.text, &mut previous.text);
        self.editor.swap_buffer_state(&mut previous.state);
        self.active = index;
        self.install_view_raw()
    }

    pub(super) fn show_prompt(&mut self, title: &[u8]) -> io::Result<()> {
        let Some(parent) = self.windows.get(self.active).map(|w| w.id) else {
            return Err(invalid("prompt requires a window"));
        };
        let existing = self.windows[self.active].prompt.is_some()
            && !self.executing_prompts.values().any(|id| *id == parent);
        let buffer = if let Some(buffer) = self.prompt_buffer {
            buffer
        } else {
            let buffer = self.buffers.len();
            self.buffers.push(Some(Buffer {
                text: Text::new(),
                state: BufferState::default(),
                document: None,
                read_only: false,
                stdout: false,
                save_method: SaveMethod::Auto,
                internal: true,
            }));
            self.prompt_buffer = Some(buffer);
            buffer
        };
        self.edit_prompt_history(buffer, |text| {
            remove_empty_line(text)?;
            let mut line = title.to_vec();
            line.push(b'\n');
            text.insert(text.len(), &line)
        })?;
        if !existing {
            let mode = self.editor.mode;
            let action = self.editor.take_prompt_action_state();
            let id = self.add_window(buffer, ViewState::initial());
            let index = self
                .windows
                .iter()
                .position(|window| window.id == id)
                .expect("new window");
            self.windows[index].prompt = Some(PromptSession {
                parent: Some(parent),
                parent_mode: mode,
                action: Some(action),
            });
            self.set_window_display(
                id,
                WindowDisplay {
                    oneline: true,
                    statusbar: false,
                    showeof: false,
                    ..WindowDisplay::default()
                },
            )?;
            self.focus_window(id)?;
            let window = Some(id as u64);
            for mode in [Mode::Normal, Mode::Insert, Mode::Visual, Mode::VisualLine] {
                self.editor
                    .map_alias(mode, "<Enter>", "<vis-prompt-enter>", window)
                    .map_err(|e| invalid(e.to_string()))?;
            }
            for (mode, left, right) in [
                (Mode::Insert, "<C-j>", "<vis-prompt-enter>"),
                (Mode::Normal, "<Escape>", "<vis-prompt-hide>"),
                (Mode::Insert, "<Up>", "<vis-prompt-expand>"),
            ] {
                self.editor
                    .map_alias(mode, left, right, window)
                    .map_err(|e| invalid(e.to_string()))?;
            }
            #[cfg(feature = "lua")]
            self.editor
                .map_alias(Mode::Insert, "<Tab>", "<C-x><C-o>", window)
                .map_err(|e| invalid(e.to_string()))?;
        }
        let end = self.editor.text.len().saturating_sub(1);
        let primary = self.editor.selection_ids()[self.editor.primary_index()];
        self.editor
            .update_selection(
                primary,
                Selection {
                    anchor: end,
                    cursor: end,
                },
            )
            .map_err(|e| invalid(e.to_string()))?;
        self.editor
            .switch_mode(Mode::Insert)
            .map_err(|e| invalid(e.to_string()))
    }

    fn edit_prompt_history<R>(
        &mut self,
        buffer: usize,
        edit: impl FnOnce(&mut Text) -> Result<R, crate::text::TextError>,
    ) -> io::Result<R> {
        self.with_buffer_mut(buffer, edit)
            .ok_or_else(|| invalid("prompt file is closed"))?
            .map_err(|e| invalid(e.to_string()))
    }

    fn restore_prompt_parent(&mut self, window: usize) -> io::Result<()> {
        let Some(index) = self.windows.iter().position(|w| w.id == window) else {
            return Ok(());
        };
        let Some(session) = self.windows[index].prompt.as_mut() else {
            return Ok(());
        };
        let parent = session.parent;
        let mode = session.parent_mode;
        let action = session.action.take();
        let target = parent
            .and_then(|id| self.windows.iter().position(|w| w.id == id))
            .or_else(|| {
                self.windows
                    .iter()
                    .position(|w| w.id != window && w.prompt.is_none())
            });
        if let Some(target) = target {
            self.focus_raw(target)?;
            if let Some(action) = action {
                self.editor.restore_prompt_action_state(action);
            }
            self.editor.set_mode_raw(mode);
        }
        Ok(())
    }

    pub(super) fn cancel_prompt(&mut self) -> io::Result<()> {
        let Some(window) = self
            .windows
            .get(self.active)
            .filter(|w| w.prompt.is_some())
            .map(|w| w.id)
        else {
            return Ok(());
        };
        if self.editor.selections().len() > 1 {
            // The ordinary normal-mode cancellation removes secondary cursors.
            let primary = self.editor.selection_ids()[self.editor.primary_index()];
            for id in self
                .editor
                .selection_ids()
                .to_vec()
                .into_iter()
                .filter(|id| *id != primary)
            {
                self.editor
                    .remove_selection(id)
                    .map_err(|e| invalid(e.to_string()))?;
            }
            return Ok(());
        }
        self.restore_prompt_parent(window)?;
        self.hide_prompt(window)
    }

    pub(super) fn expand_prompt_history(&mut self) -> io::Result<()> {
        let Some(window) = self
            .windows
            .get(self.active)
            .filter(|w| w.prompt.is_some())
            .map(|w| w.id)
        else {
            return Ok(());
        };
        self.editor
            .prompt_history_up()
            .map_err(|e| invalid(e.to_string()))?;
        self.editor
            .unmap(Mode::Insert, "<Up>", Some(window as u64))
            .map_err(|e| invalid(e.to_string()))?;
        self.windows[self.active].display = WindowDisplay {
            oneline: false,
            statusbar: false,
            showeof: true,
            ..WindowDisplay::default()
        };
        Ok(())
    }

    pub(super) fn prepare_prompt_submission(&mut self) -> io::Result<Option<Submission>> {
        let Some(window) = self
            .windows
            .get(self.active)
            .filter(|w| w.prompt.is_some())
            .map(|w| w.id)
        else {
            return Ok(None);
        };
        let buffer = self.windows[self.active].buffer;
        let data = self.editor.text.to_vec();
        let primary = self.editor.selection_ids()[self.editor.primary_index()];
        let range = if self.editor.mode == Mode::VisualLine {
            self.editor.selection_range(primary).and_then(|_| {
                self.editor
                    .selection_ranges()
                    .get(self.editor.primary_index())
                    .cloned()
            })
        } else if self.editor.mode == Mode::Visual {
            self.editor.selection_range(primary)
        } else {
            let position = self.editor.cursor.min(data.len());
            Some(
                crate::motion::line_begin(&data, position)
                    ..crate::motion::line_next(&data, position),
            )
        };
        let Some(range) = range.filter(|r| r.start <= r.end && r.end <= data.len()) else {
            self.restore_prompt_parent(window)?;
            self.hide_prompt(window)?;
            return Err(invalid("failed to detect command"));
        };
        let mut command = data[range.clone()].to_vec();
        if command.last() == Some(&b'\n') {
            command.pop();
        }
        if let Some(nul) = command.iter().position(|b| *b == 0) {
            command.truncate(nul);
        }
        self.next_prompt = self
            .next_prompt
            .checked_add(1)
            .ok_or_else(|| invalid("prompt token limit exceeded"))?;
        let token = self.next_prompt;
        let submission = Submission {
            token,
            window,
            buffer,
            range: self
                .editor
                .text
                .mark(range.start)
                .zip(self.editor.text.mark(range.end)),
            command,
            lastline: range.end == data.len(),
        };
        self.restore_prompt_parent(window)?;
        self.executing_prompts.insert(token, window);
        Ok(Some(submission))
    }

    pub(super) fn finish_prompt_submission(
        &mut self,
        submission: &Submission,
        success: bool,
    ) -> io::Result<()> {
        self.executing_prompts.remove(&submission.token);
        let Some(index) = self.windows.iter().position(|w| w.id == submission.window) else {
            return Ok(());
        };
        if success {
            // Resolve before hiding removes the draft immediately following an
            // older history line: its end mark may lie on that deletion edge.
            let history_range = submission.range.as_ref().and_then(|(start, end)| {
                self.with_buffer(submission.buffer, |text| {
                    text.mark_get(start.clone())
                        .zip(text.mark_get(end.clone()))
                        .and_then(|(start, end)| (start <= end).then_some(start..end))
                })
                .flatten()
            });
            self.hide_prompt(submission.window)?;
            if !submission.lastline {
                self.edit_prompt_history(submission.buffer, |text| {
                    if let Some(range) = history_range.filter(|r| r.end <= text.len()) {
                        text.delete(range)?;
                    }
                    let mut line = submission.command.clone();
                    line.push(b'\n');
                    text.insert(text.len(), &line)
                })?;
            }
        } else {
            let action = self.editor.take_prompt_action_state();
            if let Some(session) = self.windows[index].prompt.as_mut() {
                session.action = Some(action);
            }
            self.focus_raw(index)?;
            self.editor.set_mode_raw(Mode::Insert);
        }
        Ok(())
    }

    fn hide_prompt(&mut self, window: usize) -> io::Result<()> {
        let Some(index) = self.windows.iter().position(|w| w.id == window) else {
            return Ok(());
        };
        let buffer = self.windows[index].buffer;
        self.edit_prompt_history(buffer, |text| {
            if !text.is_empty() && text.byte(text.len() - 1) != Some(b'\n') {
                text.insert(text.len(), b"\n")?;
            }
            remove_empty_line(text)
        })?;
        for mode in [
            Mode::Normal,
            Mode::Insert,
            Mode::Visual,
            Mode::VisualLine,
            Mode::OperatorPending,
            Mode::Replace,
        ] {
            let mappings = self.editor.mapping_aliases(mode, Some(window as u64));
            for (left, _, _) in mappings {
                self.editor
                    .unmap(mode, &left, Some(window as u64))
                    .map_err(|e| invalid(e.to_string()))?;
            }
        }
        self.remove_window(index)
    }

    pub fn window_display(&self, id: usize) -> Option<WindowDisplay> {
        self.windows.iter().find(|w| w.id == id).map(|w| w.display)
    }

    pub fn set_window_display(&mut self, id: usize, display: WindowDisplay) -> io::Result<()> {
        let index = self
            .windows
            .iter()
            .position(|window| window.id == id)
            .ok_or_else(|| invalid("closed window"))?;
        self.windows[index].display = display;
        if display.oneline && index + 1 < self.windows.len() {
            let active = self.active_window_id();
            let window = self.windows.remove(index);
            self.windows.push(window);
            self.active = self
                .windows
                .iter()
                .position(|window| window.id == active)
                .expect("active window");
        }
        Ok(())
    }
}

fn remove_empty_line(text: &mut Text) -> Result<(), crate::text::TextError> {
    if text.is_empty() {
        return Ok(());
    }
    let data = text.to_vec();
    let start = crate::motion::line_begin(&data, data.len() - 1);
    let line = &data[start..];
    if matches!(line, b"\n" | b":" | b"/" | b"?" | b":\n" | b"/\n" | b"?\n") {
        text.delete(start..data.len())?;
    }
    Ok(())
}

/*
 * Structural-command semantics derived from sam.c.
 * Copyright © 2016-2020 Marc André Tanner <mat at brain-dump.org>
 * Copyright © 1998 by Lucent Technologies
 *
 * Permission to use, copy, modify, and distribute this software for any
 * purpose without fee is hereby granted, provided that this entire notice
 * is included in all copies of any software which is or includes a copy
 * or modification of this software and in all copies of the supporting
 * documentation for such software.
 *
 * THIS SOFTWARE IS BEING PROVIDED "AS IS", WITHOUT ANY EXPRESS OR IMPLIED
 * WARRANTY. IN PARTICULAR, NEITHER THE AUTHORS NOR LUCENT TECHNOLOGIES MAKE ANY
 * REPRESENTATION OR WARRANTY OF ANY KIND CONCERNING THE MERCHANTABILITY
 * OF THIS SOFTWARE OR ITS FITNESS FOR ANY PARTICULAR PURPOSE.
 */
//! Structural commands, evaluated against one immutable document snapshot.
//!
//! Like `sam.c`, groups queue edits in original byte coordinates, reject
//! overlapping changes, and apply the transcript only after successful
//! evaluation. `p` produces selections rather than writing to standard output.
//! Shell and file reads run during evaluation; their external effects cannot
//! be rolled back if a later command fails. Document edits remain transactional.
//! Session registers and stable per-document marks survive separate commands.
//! Substitution remains unsupported, matching C vis; use `x/.../ c/.../`.

use crate::regex::{Match, Regex};
use crate::text::{Mark, Text};
use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::{self, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
#[derive(Clone)]
struct ProcessGroupObserver(Arc<dyn Fn(Option<u32>) -> io::Result<()> + Send + Sync>);

#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
impl fmt::Debug for ProcessGroupObserver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProcessGroupObserver(..)")
    }
}

const MAX_DEPTH: usize = 64;
const MAX_STEPS: usize = 1_000_000;
const MAX_EXPANDED_TEXT: usize = 64 * 1024 * 1024;

/// Interactive hosts treat evaluation failures as handled commands with a
/// diagnostic. Parse, validation, and transcript failures reject the command.
/// Every phase still returns Err from the strict transactional execution API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SamErrorPhase {
    Parse,
    Validate,
    Transcript,
    Evaluation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamError {
    pub offset: usize,
    pub message: String,
    phase: SamErrorPhase,
}

impl SamError {
    fn new(offset: usize, message: impl Into<String>) -> Self {
        Self {
            offset,
            message: message.into(),
            phase: SamErrorPhase::Evaluation,
        }
    }

    fn in_phase(mut self, phase: SamErrorPhase) -> Self {
        self.phase = phase;
        self
    }

    pub fn phase(&self) -> SamErrorPhase {
        self.phase
    }

    pub fn is_evaluation(&self) -> bool {
        self.phase == SamErrorPhase::Evaluation
    }
}

impl fmt::Display for SamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at script byte {}", self.message, self.offset)
    }
}

impl std::error::Error for SamError {}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Execution {
    /// Selected ranges after all queued edits have been applied.
    pub selections: Vec<Range<usize>>,
    /// Input selection index corresponding to each selected result range.
    pub selection_inputs: Vec<usize>,
    /// Aligned with selections: groups/extractions create new logical selection
    /// objects, while direct p (including guards) updates its input selection.
    pub selection_detached: Vec<bool>,
    /// Every transcript effect, in file order, for mode-specific cursor and
    /// selection updates by the editor. Ranges refer to the final document.
    pub changes: Vec<Change>,
    /// Reserved for commands which explicitly produce output. `p` selects.
    pub output: Vec<u8>,
}

/// A write request resolved without mutating text or editor selections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRequest {
    pub force: bool,
    pub quit: bool,
    /// Original quoted filename argument tail; the host resolves paths.
    pub path: Option<String>,
    /// None writes the whole buffer; Some concatenates these raw byte ranges.
    pub ranges: Option<Vec<Range<usize>>>,
    /// Visual writes without an explicit filename require force even if their
    /// selection spans the entire document, as in C vis.
    pub visual: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// Byte range before this script.
    pub source: Range<usize>,
    /// Inserted/replacement bytes in the final document; deletions are empty.
    pub range: Range<usize>,
    /// True for edits with an insertion component, including an empty `c`.
    pub insertion: bool,
    pub contains_newline: bool,
    /// True when a group or extraction creates a result selection instead of
    /// updating the selection on which the script was invoked. Guards retain
    /// the context of their parent command.
    pub detached: bool,
    /// Index of the original input selection which produced this edit.
    pub input: usize,
}

/// Editor-wide regex/shell registers, directory, and per-document mark sets.
/// Parsing a valid regex or shell command updates its register immediately,
/// including when a later part of the script fails to parse, as in C vis.
#[derive(Debug)]
pub struct Session {
    previous_regex: Option<String>,
    previous_shell: Option<String>,
    /// Native `shell` option: the program used for external commands.
    shell: PathBuf,
    documents: Vec<(Mark, BTreeMap<char, Vec<Mark>>)>,
    directory: Option<PathBuf>,
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    interrupt: Option<Arc<AtomicBool>>,
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    process_group_observer: Option<ProcessGroupObserver>,
}

/// Native `shell` default: the environment `SHELL`, then `/bin/sh`.
fn default_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

impl Default for Session {
    fn default() -> Self {
        Self {
            previous_regex: None,
            previous_shell: None,
            shell: default_shell(),
            documents: Vec::new(),
            directory: None,
            #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
            interrupt: None,
            #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
            process_group_observer: None,
        }
    }
}

#[derive(Clone, Copy)]
struct ShellContext<'a> {
    directory: Option<&'a Path>,
    /// Native `shell` option: the program used for external commands.
    shell: &'a Path,
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    interrupt: Option<&'a AtomicBool>,
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    process_group_observer: Option<&'a ProcessGroupObserver>,
}

impl ShellContext<'_> {
    fn interrupted(self) -> bool {
        #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
        if self
            .interrupt
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return true;
        }
        false
    }
}

fn valid_mark_name(name: char) -> bool {
    name.is_ascii_lowercase() || matches!(name, '\'' | '^')
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn previous_regex(&self) -> Option<&str> {
        self.previous_regex.as_deref()
    }
    pub fn previous_shell(&self) -> Option<&str> {
        self.previous_shell.as_deref()
    }

    /// Native `shell` option. Without it the environment `SHELL` is used, and
    /// `/bin/sh` is the last resort.
    pub fn set_shell(&mut self, shell: Option<String>) {
        self.shell = match shell {
            Some(shell) if !shell.is_empty() => PathBuf::from(shell),
            _ => default_shell(),
        };
    }

    /// The program used to run external commands.
    pub fn shell_path(&self) -> &Path {
        &self.shell
    }

    pub fn set_previous_regex(&mut self, pattern: &str) -> Result<(), SamError> {
        Regex::new(pattern).map_err(|e| {
            SamError::new(0, format!("invalid regular expression: {e}"))
                .in_phase(SamErrorPhase::Parse)
        })?;
        self.previous_regex = Some(pattern.to_owned());
        Ok(())
    }

    /// Relative file reads and subprocesses use this directory. Leaving it unset
    /// inherits the process directory without ever mutating global state.
    pub fn set_directory(&mut self, directory: PathBuf) {
        self.directory = Some(directory);
    }
    pub fn directory(&self) -> Option<&Path> {
        self.directory.as_deref()
    }

    /// The host sets this flag for external signals and clears it after handling
    /// cancellation. A process-group observer can give a shell foreground
    /// terminal control; hosts without that facility set this shared flag.
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    pub fn set_interrupt(&mut self, interrupt: Arc<AtomicBool>) {
        self.interrupt = Some(interrupt);
    }

    /// Give a newly spawned shell group foreground terminal control, then
    /// restore the host after group cleanup and reaping. A failing Some callback
    /// must support None restoration even when it transferred control partially.
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    pub fn set_process_group_observer(
        &mut self,
        observer: Arc<dyn Fn(Option<u32>) -> io::Result<()> + Send + Sync>,
    ) {
        self.process_group_observer = Some(ProcessGroupObserver(observer));
    }

    pub fn set_marks(&mut self, text: &Text, name: char, marks: Vec<Mark>) -> Result<(), SamError> {
        if !valid_mark_name(name) {
            return Err(SamError::new(0, "invalid mark name").in_phase(SamErrorPhase::Validate));
        }
        let identity = text.mark(text.len()).expect("EOF is a valid text mark");
        if let Some((_, document)) = self
            .documents
            .iter_mut()
            .find(|(owner, _)| *owner == identity)
        {
            document.insert(name, marks);
        } else {
            let mut document = BTreeMap::new();
            document.insert(name, marks);
            self.documents.push((identity, document));
        }
        Ok(())
    }

    pub fn marks(&self, text: &Text, name: char) -> Option<&[Mark]> {
        let identity = text.mark(text.len())?;
        self.documents
            .iter()
            .find(|(owner, _)| *owner == identity)?
            .1
            .get(&name)
            .map(Vec::as_slice)
    }

    pub fn clear_marks(&mut self, text: &Text, name: char) -> Option<Vec<Mark>> {
        let identity = text.mark(text.len())?;
        self.documents
            .iter_mut()
            .find(|(owner, _)| *owner == identity)?
            .1
            .remove(&name)
    }

    pub fn close_text_marks(&mut self, text: &Text) {
        self.forget_document(text);
    }

    /// Release all mark owners and backing byte blocks when the host closes a
    /// buffer; regex/shell registers and other documents remain available.
    pub fn forget_document(&mut self, text: &Text) {
        if let Some(identity) = text.mark(text.len()) {
            self.documents.retain(|(owner, _)| *owner != identity);
        }
    }

    pub fn execute(
        &mut self,
        text: &mut Text,
        script: &str,
        dot: Range<usize>,
    ) -> Result<Execution, SamError> {
        self.execute_in_mode(text, script, &[dot], false)
    }

    /// Evaluate one parsed command tree over all original-coordinate selections
    /// before applying a single transcript. Guard counts span the inputs; nested
    /// x/y loops initialize their own count domains for each input.
    pub fn execute_many(
        &mut self,
        text: &mut Text,
        script: &str,
        dots: &[Range<usize>],
    ) -> Result<Execution, SamError> {
        self.execute_in_mode(text, script, dots, dots.iter().any(|dot| !dot.is_empty()))
    }

    fn resolved_marks(&self, text: &Text) -> BTreeMap<char, Vec<Option<usize>>> {
        let identity = text.mark(text.len());
        self.documents
            .iter()
            .find(|(owner, _)| Some(owner) == identity.as_ref())
            .map(|(_, marks)| {
                marks
                    .iter()
                    .map(|(&name, marks)| {
                        (
                            name,
                            marks
                                .iter()
                                .map(|mark| text.mark_get(mark.clone()))
                                .collect(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn shell_context(&self) -> ShellContext<'_> {
        ShellContext {
            directory: self.directory.as_deref(),
            shell: self.shell_path(),
            #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
            interrupt: self.interrupt.as_deref(),
            #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
            process_group_observer: self.process_group_observer.as_ref(),
        }
    }

    /// Resolve addressed w/write/wq for the host's file writer. Unknown commands
    /// return None. Address regexes update the search register during parsing,
    /// including when a later filename or address error rejects the write.
    pub fn resolve_write(
        &mut self,
        text: &Text,
        script: &str,
        dots: &[Range<usize>],
        visual: bool,
    ) -> Result<Option<WriteRequest>, SamError> {
        let mut parser = Parser {
            script: script.as_bytes(),
            pos: 0,
            previous_regex: self.previous_regex.clone(),
            previous_shell: self.previous_shell.clone(),
            host_commands: None,
            saw_file_loop: false,
            saw_user: false,
        };
        let parsed = parser.write();
        self.previous_regex = parser.previous_regex;
        self.previous_shell = parser.previous_shell;
        let Some(parsed) = parsed.map_err(|error| error.in_phase(SamErrorPhase::Parse))? else {
            return Ok(None);
        };
        if dots.is_empty()
            || dots.len() > MAX_STEPS
            || dots
                .iter()
                .any(|dot| dot.start > dot.end || dot.end > text.len())
        {
            return Err(
                SamError::new(0, "invalid initial selection").in_phase(SamErrorPhase::Validate)
            );
        }
        let data = text.to_vec();
        let marks = self.resolved_marks(text);
        let mut evaluator = Evaluator {
            data: &data,
            edits: Vec::new(),
            selections: Vec::new(),
            selection_inputs: Vec::new(),
            selection_detached: Vec::new(),
            captures: Vec::new(),
            capture_values: None,
            steps: 0,
            expanded_bytes: 0,
            marks: &marks,
            input: 0,
            target_window: None,
            target_selection: None,
            shell: self.shell_context(),
        };
        let addressed = parsed
            .address
            .as_ref()
            .map(|address| evaluator.address(address, dots[0].clone(), 0))
            .transpose()?;
        // C w runs once. Visual mode streams all original selections, even when
        // an address was also supplied; nonvisual mode writes its one address.
        let ranges = if visual {
            Some(dots.to_vec())
        } else {
            addressed.map(|range| vec![range])
        };
        Ok(Some(WriteRequest {
            force: parsed.force,
            quit: parsed.quit,
            path: parsed.path,
            ranges,
            visual,
        }))
    }

    /// Parse a host-capable program once, retaining shared count state across
    /// window visits. Evaluation yields owned requests and never borrows a host.
    pub fn parse_program(
        &mut self,
        script: &str,
        external_commands: &[String],
    ) -> Result<Program, SamError> {
        let mut parser = Parser {
            script: script.as_bytes(),
            pos: 0,
            previous_regex: self.previous_regex.clone(),
            previous_shell: self.previous_shell.clone(),
            host_commands: Some(external_commands),
            saw_file_loop: false,
            saw_user: false,
        };
        let commands = parser.parse();
        self.previous_regex = parser.previous_regex;
        self.previous_shell = parser.previous_shell;
        let commands = commands.map_err(|error| error.in_phase(SamErrorPhase::Parse))?;
        Self::checked_program(commands)
    }

    /// Probe for workspace file loops or registered user commands without
    /// changing registers for commands owned by the regular editor. Once either
    /// host construct is recognized, retain all normal
    /// parser register side effects even if a later part of the program fails.
    pub fn parse_workspace_program(
        &mut self,
        script: &str,
        external_commands: &[String],
    ) -> Result<Option<Program>, SamError> {
        let mut parser = Parser {
            script: script.as_bytes(),
            pos: 0,
            previous_regex: self.previous_regex.clone(),
            previous_shell: self.previous_shell.clone(),
            host_commands: Some(external_commands),
            saw_file_loop: false,
            saw_user: false,
        };
        let result = parser.parse();
        if !parser.saw_file_loop && !parser.saw_user {
            return Ok(None);
        }
        self.previous_regex = parser.previous_regex;
        self.previous_shell = parser.previous_shell;
        let commands = result.map_err(|error| error.in_phase(SamErrorPhase::Parse))?;
        Self::checked_program(commands).map(Some)
    }

    fn checked_program(commands: Vec<Command>) -> Result<Program, SamError> {
        if commands.len() != 1 {
            return Err(
                SamError::new(0, "expected one command or an explicit group")
                    .in_phase(SamErrorPhase::Parse),
            );
        }
        fn validate(command: &Command, in_loop: bool, in_group: bool) -> Result<(), SamError> {
            if matches!(&command.operation, Operation::Host { command, .. } if command.destructive())
                && (in_loop || in_group)
            {
                return Err(SamError::new(
                    command.offset,
                    "destructive command inside a structural loop or group",
                )
                .in_phase(SamErrorPhase::Validate));
            }
            let in_loop = in_loop || matches!(command.operation, Operation::Extract { .. });
            let in_group = in_group || matches!(command.operation, Operation::Group(_));
            for child in command.children() {
                validate(child, in_loop, in_group)?;
            }
            Ok(())
        }
        validate(&commands[0], false, false)?;
        Ok(Program { commands })
    }

    /// Explicit mode context for hosts which distinguish an unanchored cursor
    /// range from a visual selection. Single-buffer `execute` starts nonvisual;
    /// `execute_many` infers visual mode from nonempty input ranges.
    pub fn execute_in_mode(
        &mut self,
        text: &mut Text,
        script: &str,
        dots: &[Range<usize>],
        visual: bool,
    ) -> Result<Execution, SamError> {
        let mut parser = Parser {
            script: script.as_bytes(),
            pos: 0,
            previous_regex: self.previous_regex.clone(),
            previous_shell: self.previous_shell.clone(),
            host_commands: None,
            saw_file_loop: false,
            saw_user: false,
        };
        let commands = parser.parse();
        self.previous_regex = parser.previous_regex;
        self.previous_shell = parser.previous_shell;
        let commands = commands.map_err(|error| error.in_phase(SamErrorPhase::Parse))?;
        let positions = self.resolved_marks(text);
        execute_commands(
            text,
            &commands,
            dots,
            &positions,
            self.shell_context(),
            visual,
        )
    }
}

/// Host operations parsed once alongside structural commands. Arguments are
/// already unquoted; adapters must execute these values without reparsing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCommand {
    Write,
    WriteQuit,
    Quit,
    QuitAll,
    Edit,
    Open,
    Split,
    VSplit,
    New,
    VNew,
    ChangeDirectory,
    Set,
    Map,
    Unmap,
    MapWindow,
    UnmapWindow,
    LangMap,
    Earlier,
    Later,
    Help,
    Substitute,
    Buffer,
    BufferDelete,
    Files,
    WorkingDirectory,
    WindowCommand,
    User(String),
}
impl HostCommand {
    pub fn name(&self) -> &str {
        match self {
            Self::Write => "w",
            Self::WriteQuit => "wq",
            Self::Quit => "q",
            Self::QuitAll => "qall",
            Self::Edit => "e",
            Self::Open => "open",
            Self::Split => "split",
            Self::VSplit => "vsplit",
            Self::New => "new",
            Self::VNew => "vnew",
            Self::ChangeDirectory => "cd",
            Self::Set => "set",
            Self::Map => "map",
            Self::Unmap => "unmap",
            Self::MapWindow => "map-window",
            Self::UnmapWindow => "unmap-window",
            Self::LangMap => "langmap",
            Self::Earlier => "earlier",
            Self::Later => "later",
            Self::Help => "help",
            Self::Substitute => "s",
            Self::Buffer => "buffer",
            Self::BufferDelete => "bdelete",
            Self::Files => "files",
            Self::WorkingDirectory => "pwd",
            Self::WindowCommand => "wincmd",
            Self::User(name) => name,
        }
    }
    fn lookup(name: &str, external: &[String]) -> Option<Self> {
        let exact = match name {
            "w" | "write" => Some(Self::Write),
            "wq" => Some(Self::WriteQuit),
            "q" | "quit" => Some(Self::Quit),
            "qall" => Some(Self::QuitAll),
            "e" | "edit" => Some(Self::Edit),
            "open" | "o" => Some(Self::Open),
            "split" => Some(Self::Split),
            "vsplit" => Some(Self::VSplit),
            "new" => Some(Self::New),
            "vnew" => Some(Self::VNew),
            "cd" | "chdir" => Some(Self::ChangeDirectory),
            "set" => Some(Self::Set),
            "map" => Some(Self::Map),
            "unmap" => Some(Self::Unmap),
            "map-window" => Some(Self::MapWindow),
            "unmap-window" => Some(Self::UnmapWindow),
            "langmap" => Some(Self::LangMap),
            "earlier" => Some(Self::Earlier),
            "later" => Some(Self::Later),
            "help" => Some(Self::Help),
            "s" => Some(Self::Substitute),
            "b" | "B" | "buffer" => Some(Self::Buffer),
            "D" | "bdelete" => Some(Self::BufferDelete),
            "ls" | "files" => Some(Self::Files),
            "pwd" => Some(Self::WorkingDirectory),
            "wincmd" => Some(Self::WindowCommand),
            _ => None,
        };
        if external.iter().any(|candidate| candidate == name) {
            return Some(Self::User(name.to_owned()));
        }
        if exact.is_some() {
            return exact;
        }
        if matches!(
            name,
            "a" | "c" | "d" | "g" | "i" | "p" | "r" | "v" | "x" | "y" | "X" | "Y"
        ) {
            return None;
        }
        let mut candidates = [
            "w",
            "wq",
            "q",
            "qall",
            "e",
            "open",
            "split",
            "vsplit",
            "new",
            "vnew",
            "cd",
            "set",
            "map",
            "unmap",
            "map-window",
            "unmap-window",
            "langmap",
            "earlier",
            "later",
            "help",
        ]
        .into_iter()
        .chain(external.iter().map(String::as_str))
        .filter(|candidate| candidate.starts_with(name))
        .collect::<BTreeSet<_>>()
        .into_iter();
        let candidate = candidates.next()?;
        if candidates.next().is_some() {
            return None;
        }
        if external.iter().any(|registered| registered == candidate) {
            // C resolves metadata by unique prefix, but invokes a user handler
            // by the original argv[0]. Its abbreviated handler lookup can fail.
            Some(Self::User(name.to_owned()))
        } else {
            Self::lookup(candidate, external)
        }
    }
    fn destructive(&self) -> bool {
        matches!(
            self,
            Self::Quit | Self::QuitAll | Self::Edit | Self::WriteQuit | Self::BufferDelete
        )
    }
    fn permits_address(&self) -> bool {
        matches!(
            self,
            Self::Write | Self::WriteQuit | Self::Substitute | Self::User(_)
        )
    }
    fn permits_force(&self) -> bool {
        matches!(
            self,
            Self::Write
                | Self::WriteQuit
                | Self::Quit
                | Self::QuitAll
                | Self::Edit
                | Self::Map
                | Self::MapWindow
                | Self::LangMap
                | Self::BufferDelete
                | Self::User(_)
        )
    }
}

#[derive(Debug)]
pub struct Program {
    commands: Vec<Command>,
}
impl Program {
    pub fn has_file_loops(&self) -> bool {
        fn contains(command: &Command) -> bool {
            matches!(command.operation, Operation::FileLoop { .. })
                || command.children().iter().any(contains)
        }
        self.commands.iter().any(contains)
    }
    pub fn requires_host(&self) -> bool {
        fn contains(command: &Command) -> bool {
            matches!(
                command.operation,
                Operation::FileLoop { .. } | Operation::Host { .. }
            ) || command.children().iter().any(contains)
        }
        self.commands.iter().any(contains)
    }
    fn node(&self, path: &[usize]) -> &Command {
        let mut command = &self.commands[path[0]];
        for &index in &path[1..] {
            command = &command.children()[index];
        }
        command
    }
}

#[derive(Debug, Clone)]
pub struct SelectionInput {
    pub id: u64,
    pub range: Range<usize>,
}
#[derive(Clone, Copy)]
pub struct WindowContext<'a> {
    pub window: usize,
    pub file: usize,
    pub text: &'a Text,
    pub selections: &'a [SelectionInput],
    pub visual: bool,
}
#[derive(Debug, Clone)]
pub struct FileLoopRequest {
    pub pattern: Option<String>,
    pub invert: bool,
}
impl FileLoopRequest {
    pub fn matches(&self, path: &[u8]) -> Result<bool, SamError> {
        let matched = match &self.pattern {
            None => true,
            Some(_) if path.is_empty() => false,
            Some(pattern) => Regex::new(pattern)
                .and_then(|regex| regex.find(path, 0..path.len()))
                .map_err(|error| {
                    SamError::new(0, error.to_string()).in_phase(SamErrorPhase::Transcript)
                })?
                .is_some(),
        };
        Ok(matched != self.invert)
    }
}
#[derive(Debug, Clone)]
pub struct HostRequest {
    pub window: usize,
    pub file: usize,
    pub command: HostCommand,
    pub force: bool,
    pub args: Vec<String>,
    pub range: Range<usize>,
    pub ranges: Vec<Range<usize>>,
    pub visual: bool,
    pub selection: Option<u64>,
}
#[derive(Debug, Clone)]
pub enum SelectionEffect {
    Print {
        source: Option<u64>,
        range: Range<usize>,
    },
    Dispose {
        source: u64,
    },
}
#[derive(Debug, Clone)]
pub struct SelectionRequest {
    pub window: usize,
    pub file: usize,
    pub effect: SelectionEffect,
}
#[derive(Debug, Clone)]
pub struct CommittedChange {
    pub window: usize,
    pub selection: Option<u64>,
    pub change: Change,
}
#[derive(Debug, Clone)]
pub enum SamStep {
    NeedWindow(usize),
    Files(FileLoopRequest),
    Host(HostRequest),
    Selection(SelectionRequest),
    Diagnostic(SamError),
    Complete,
}

#[derive(Debug)]
struct Count {
    modulo: bool,
    resolved_start: Cell<i64>,
    resolved_end: Cell<i64>,
    iteration: Cell<i64>,
}

impl Count {
    fn new(start: i64, end: i64, modulo: bool) -> Self {
        Self {
            modulo,
            resolved_start: Cell::new(start),
            resolved_end: Cell::new(end),
            iteration: Cell::new(0),
        }
    }
    fn reset(&self, domain: Option<usize>) {
        let resolve = |number| {
            if number < 0 {
                domain.map_or(number, |total| number + total as i64 + 1)
            } else {
                number
            }
        };
        self.resolved_start.set(resolve(self.resolved_start.get()));
        self.resolved_end.set(resolve(self.resolved_end.get()));
        self.iteration.set(0);
    }
    fn next_matches(&self) -> bool {
        let iteration = self.iteration.get() + 1;
        self.iteration.set(iteration);
        let start = self.resolved_start.get();
        if self.modulo {
            start == 0 || iteration % start == 0
        } else {
            start <= iteration && iteration <= self.resolved_end.get()
        }
    }
}

#[derive(Debug)]
enum Atom {
    Dot,
    End,
    All,
    Byte(usize),
    Mark(char),
    Line(usize, Cell<bool>),
    Search { pattern: String, backwards: bool },
    Plus,
    Minus,
}

#[derive(Debug)]
enum Address {
    Chain(Vec<Atom>),
    Span {
        left: Option<Box<Address>>,
        right: Option<Box<Address>>,
        semicolon: bool,
    },
}

struct ParsedWrite {
    address: Option<Address>,
    force: bool,
    quit: bool,
    path: Option<String>,
}

#[derive(Debug)]
enum Operation {
    Insert {
        at_end: bool,
        text: Vec<u8>,
        count: usize,
    },
    Change {
        text: Vec<u8>,
        count: usize,
    },
    Delete,
    Select,
    Group(Vec<Command>),
    Extract {
        pattern: Option<String>,
        complement: bool,
        command: Box<Command>,
    },
    Guard {
        pattern: Option<String>,
        count: Count,
        invert: bool,
        command: Box<Command>,
    },
    Shell {
        kind: u8,
        command: String,
    },
    Read(Vec<String>),
    FileLoop {
        pattern: Option<String>,
        invert: bool,
        command: Box<Command>,
    },
    Host {
        command: HostCommand,
        force: bool,
        args: Vec<String>,
    },
}

#[derive(Debug)]
struct Command {
    address: Option<Address>,
    operation: Operation,
    offset: usize,
}

impl Command {
    fn children(&self) -> &[Command] {
        match &self.operation {
            Operation::Guard { command, .. }
            | Operation::Extract { command, .. }
            | Operation::FileLoop { command, .. } => std::slice::from_ref(command),
            Operation::Group(commands) => commands,
            _ => &[],
        }
    }
    fn reset_counts(&self, domain: Option<usize>) {
        if let Operation::Guard { count, .. } = &self.operation {
            count.reset(domain);
        }
        if matches!(self.operation, Operation::FileLoop { .. }) {
            return;
        }
        for child in self.children() {
            // C skips child extraction nodes, but includes the root of the
            // count domain even when that root itself is an extraction.
            if !matches!(child.operation, Operation::Extract { .. }) {
                child.reset_counts(domain);
            }
        }
    }
    fn negative_count(&self) -> bool {
        if matches!(self.operation, Operation::FileLoop { .. }) {
            return false;
        }
        matches!(&self.operation, Operation::Guard { count, .. } if count.resolved_start.get() < 0 || count.resolved_end.get() < 0)
            || self
                .children()
                .iter()
                .filter(|child| !matches!(child.operation, Operation::Extract { .. }))
                .any(Command::negative_count)
    }
}

struct Parser<'a> {
    script: &'a [u8],
    pos: usize,
    previous_regex: Option<String>,
    previous_shell: Option<String>,
    host_commands: Option<&'a [String]>,
    saw_file_loop: bool,
    saw_user: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<u8> {
        self.script.get(self.pos).copied()
    }

    fn spaces(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r')) {
            self.pos += 1;
        }
    }

    fn whitespace(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn error(&self, message: impl Into<String>) -> SamError {
        SamError::new(self.pos, message)
    }

    fn number(&mut self) -> Result<Option<usize>, SamError> {
        let mut number = 0usize;
        let start = self.pos;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            number = number
                .checked_mul(10)
                .and_then(|n| n.checked_add(usize::from(b - b'0')))
                .ok_or_else(|| self.error("number is too large"))?;
            self.pos += 1;
        }
        Ok((start != self.pos).then_some(number))
    }

    fn signed_number(&mut self) -> Result<Option<i64>, SamError> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        let Some(number) = self.number()? else {
            self.pos = start;
            return Ok(None);
        };
        let number = i64::try_from(number).map_err(|_| self.error("count is too large"))?;
        Ok(Some(if negative { -number } else { number }))
    }

    fn count(&mut self) -> Result<Count, SamError> {
        if self.peek() == Some(b'%') {
            self.pos += 1;
            let number = self
                .signed_number()?
                .filter(|&number| number != 0)
                .ok_or_else(|| self.error("invalid modulo count"))?;
            return Ok(Count::new(number, number, true));
        }
        let start = self.signed_number()?;
        if start == Some(0) {
            return Err(self.error("guard count cannot be zero"));
        }
        let end = if self.peek() == Some(b',') {
            self.pos += 1;
            let end = self.signed_number()?;
            if end == Some(0) {
                return Err(self.error("guard count cannot be zero"));
            }
            end.unwrap_or(i64::MAX)
        } else {
            start.unwrap_or(i64::MAX)
        };
        Ok(Count::new(start.unwrap_or(0), end, false))
    }

    /// Retains backslashes needed by regexes and by replacement expansion.
    /// Vis permits a missing final delimiter at end of input.
    fn delimited(&mut self) -> Result<Vec<u8>, SamError> {
        let delimiter = self.peek().ok_or_else(|| self.error("missing delimiter"))?;
        if delimiter.is_ascii_whitespace() || delimiter >= 0x80 {
            return Err(self.error("expected a non-whitespace ASCII delimiter"));
        }
        self.pos += 1;
        let mut bytes = Vec::new();
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == delimiter {
                return Ok(bytes);
            }
            if b == b'\\' {
                let escaped = self
                    .peek()
                    .ok_or_else(|| self.error("trailing backslash"))?;
                self.pos += 1;
                match escaped {
                    b'\n' => {}
                    b'n' => bytes.push(b'\n'),
                    b't' => bytes.push(b'\t'),
                    c if c == delimiter => bytes.push(c),
                    c => {
                        bytes.push(b'\\');
                        bytes.push(c);
                    }
                }
            } else {
                bytes.push(b);
            }
        }
        Ok(bytes)
    }

    fn pattern(&mut self) -> Result<String, SamError> {
        let offset = self.pos;
        let bytes = self.delimited()?;
        if bytes.is_empty() {
            return self
                .previous_regex
                .clone()
                .ok_or_else(|| self.error("no previous regular expression"));
        }
        let pattern =
            String::from_utf8(bytes).map_err(|_| self.error("regular expression is not UTF-8"))?;
        Regex::new(&pattern)
            .map_err(|e| SamError::new(offset, format!("invalid regular expression: {e}")))?;
        self.previous_regex = Some(pattern.clone());
        Ok(pattern)
    }

    fn address(&mut self, depth: usize) -> Result<Option<Address>, SamError> {
        if depth > MAX_DEPTH {
            return Err(self.error("address nesting limit exceeded"));
        }
        let mut atoms = Vec::new();
        loop {
            self.spaces();
            let atom = match self.peek() {
                Some(b'.') => {
                    self.pos += 1;
                    Atom::Dot
                }
                Some(b'$') => {
                    self.pos += 1;
                    Atom::End
                }
                Some(b'%') => {
                    self.pos += 1;
                    Atom::All
                }
                Some(b'#') => {
                    self.pos += 1;
                    Atom::Byte(self.number()?.unwrap_or(0))
                }
                Some(b'0'..=b'9') => {
                    Atom::Line(self.number()?.expect("digit was present"), Cell::new(false))
                }
                Some(b'/') => Atom::Search {
                    pattern: self.pattern()?,
                    backwards: false,
                },
                Some(b'?') => Atom::Search {
                    pattern: self.pattern()?,
                    backwards: true,
                },
                Some(b'+') => {
                    self.pos += 1;
                    Atom::Plus
                }
                Some(b'-') => {
                    self.pos += 1;
                    Atom::Minus
                }
                Some(b'\'') => {
                    self.pos += 1;
                    let name = self
                        .peek()
                        .map(char::from)
                        .filter(|&name| valid_mark_name(name))
                        .ok_or_else(|| self.error("invalid mark name"))?;
                    self.pos += 1;
                    Atom::Mark(name)
                }
                _ => break,
            };
            if !atoms.is_empty() && matches!(atom, Atom::Dot | Atom::End | Atom::All) {
                return Err(self.error("invalid compound address"));
            }
            atoms.push(atom);
            if atoms.len() > MAX_STEPS {
                return Err(self.error("address size limit exceeded"));
            }
        }
        let left = (!atoms.is_empty()).then_some(Address::Chain(atoms));
        self.spaces();
        if matches!(self.peek(), Some(b',' | b';')) {
            let semicolon = self.peek() == Some(b';');
            self.pos += 1;
            let right = self.address(depth + 1)?;
            if matches!(&right, Some(Address::Span { left: None, .. })) {
                return Err(self.error("missing address between range separators"));
            }
            Ok(Some(Address::Span {
                left: left.map(Box::new),
                right: right.map(Box::new),
                semicolon,
            }))
        } else {
            Ok(left)
        }
    }

    fn text(&mut self) -> Result<(Vec<u8>, usize), SamError> {
        self.spaces();
        let count = self.number()?.unwrap_or(1);
        if self.peek() != Some(b'\n') {
            return Ok((self.delimited()?, count));
        }
        self.pos += 1;
        let start = self.pos;
        loop {
            let end = self.script[self.pos..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(self.script.len(), |n| self.pos + n);
            if &self.script[self.pos..end] == b"." {
                let text = self.script[start..self.pos].to_vec();
                self.pos = end + usize::from(end < self.script.len());
                return Ok((text, count));
            }
            if end == self.script.len() {
                return Err(self.error("missing dot terminating text"));
            }
            self.pos = end + 1;
        }
    }

    fn child(&mut self, depth: usize) -> Result<Box<Command>, SamError> {
        self.spaces();
        if matches!(self.peek(), None | Some(b'\n' | b'}')) {
            return Ok(Box::new(Command {
                address: None,
                operation: Operation::Select,
                offset: self.pos,
            }));
        }
        self.command(depth + 1).map(Box::new)
    }

    fn shell(&mut self) -> Result<String, SamError> {
        self.spaces();
        let start = self.pos;
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
        let command = std::str::from_utf8(&self.script[start..self.pos])
            .map_err(|_| self.error("shell command is not UTF-8"))?
            .trim_end()
            .to_owned();
        if command.is_empty() {
            return self
                .previous_shell
                .clone()
                .ok_or_else(|| self.error("no previous shell command"));
        }
        self.previous_shell = Some(command.clone());
        Ok(command)
    }

    fn arguments(&mut self) -> Result<Vec<String>, SamError> {
        let mut names = Vec::new();
        loop {
            self.spaces();
            if matches!(self.peek(), None | Some(b'\n' | b'}')) {
                break;
            }
            let mut name = Vec::new();
            let mut quote = None;
            while let Some(c) = self.peek() {
                if quote.is_none() && c.is_ascii_whitespace() {
                    break;
                }
                self.pos += 1;
                if quote == Some(c) {
                    quote = None;
                } else if quote.is_none() && matches!(c, b'\'' | b'"') {
                    quote = Some(c);
                } else if c == b'\\' {
                    name.push(
                        self.peek()
                            .ok_or_else(|| self.error("trailing filename escape"))?,
                    );
                    self.pos += 1;
                } else {
                    name.push(c);
                }
            }
            if quote.is_some() {
                return Err(self.error("unclosed filename quote"));
            }
            names.push(String::from_utf8(name).map_err(|_| self.error("filename is not UTF-8"))?);
        }
        Ok(names)
    }

    fn filenames(&mut self) -> Result<Vec<String>, SamError> {
        let names = self.arguments()?;
        if names.is_empty() {
            return Err(self.error("r requires a filename"));
        }
        Ok(names)
    }

    fn write(&mut self) -> Result<Option<ParsedWrite>, SamError> {
        self.whitespace();
        let address = self.address(0)?;
        self.spaces();
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'-'))
        {
            self.pos += 1;
        }
        let name = &self.script[start..self.pos];
        if !matches!(name, b"w" | b"write" | b"wq") {
            return Ok(None);
        }
        let quit = name == b"wq";
        let force = self.peek() == Some(b'!');
        if force {
            self.pos += 1;
        }
        self.spaces();
        let arguments_start = self.pos;
        let names = self.arguments()?;
        let arguments_end = self.pos;
        self.whitespace();
        if self.peek().is_some() {
            return Err(self.error("unexpected command after write"));
        }
        let path = if names.is_empty() {
            None
        } else {
            Some(
                std::str::from_utf8(&self.script[arguments_start..arguments_end])
                    .map_err(|_| self.error("filename is not UTF-8"))?
                    .trim()
                    .to_owned(),
            )
        };
        Ok(Some(ParsedWrite {
            address,
            force,
            quit,
            path,
        }))
    }

    fn command(&mut self, depth: usize) -> Result<Command, SamError> {
        if depth > MAX_DEPTH {
            return Err(self.error("command nesting limit exceeded"));
        }
        self.spaces();
        let offset = self.pos;
        let address = self.address(0)?;
        self.spaces();
        if let Some(external) = self.host_commands {
            let name_start = self.pos;
            while self.peek().is_some_and(|byte| {
                byte.is_ascii_alphabetic()
                    || byte == b'_'
                    || (byte == b'-'
                        && self.script[self.pos + 1..]
                            .iter()
                            .find(|&&next| next != b'-')
                            .is_some_and(|next| next.is_ascii_alphabetic() || *next == b'_'))
            }) {
                self.pos += 1;
            }
            let name = std::str::from_utf8(&self.script[name_start..self.pos])
                .map_err(|_| self.error("command is not UTF-8"))?;
            if !name.is_empty() {
                if let Some(command) = HostCommand::lookup(name, external) {
                    self.saw_user |= matches!(command, HostCommand::User(_));
                    if address.is_some() && !command.permits_address() {
                        return Err(self.error("command does not accept an address"));
                    }
                    let force = self.peek() == Some(b'!') && command.permits_force();
                    if force {
                        self.pos += 1;
                    }
                    let args = if command == HostCommand::Substitute {
                        Vec::new()
                    } else {
                        self.arguments()?
                    };
                    return Ok(Command {
                        address,
                        operation: Operation::Host {
                            command,
                            force,
                            args,
                        },
                        offset,
                    });
                }
                if name.len() != 1 {
                    return Err(self.error("unknown or ambiguous command"));
                }
            }
            self.pos = name_start;
        }
        let opcode = self.peek();
        if !matches!(opcode, None | Some(b'\n' | b'}')) {
            self.pos += 1;
        }
        let operation = match opcode {
            Some(b'i' | b'a') => {
                let (text, count) = self.text()?;
                Operation::Insert {
                    at_end: opcode == Some(b'a'),
                    text,
                    count,
                }
            }
            Some(b'c') => {
                let (text, count) = self.text()?;
                Operation::Change { text, count }
            }
            Some(b'd') => Operation::Delete,
            Some(b'p') | None | Some(b'\n' | b'}') => {
                if opcode != Some(b'p') && address.is_none() {
                    return Err(self.error("missing command"));
                }
                Operation::Select
            }
            Some(b'{') => {
                let mut commands = Vec::new();
                loop {
                    self.whitespace();
                    match self.peek() {
                        Some(b'}') => {
                            self.pos += 1;
                            break;
                        }
                        None => return Err(self.error("unbalanced command group")),
                        _ => commands.push(self.command(depth + 1)?),
                    }
                }
                Operation::Group(commands)
            }
            Some(b'x' | b'y') => {
                let complement = opcode == Some(b'y');
                let pattern = if matches!(self.peek(), None | Some(b' ' | b'\t' | b'\n' | b'}')) {
                    if complement {
                        return Err(self.error("y requires a regular expression"));
                    }
                    None
                } else {
                    Some(self.pattern()?)
                };
                Operation::Extract {
                    pattern,
                    complement,
                    command: self.child(depth)?,
                }
            }
            Some(b'g' | b'v') => {
                let count = self.count()?;
                let pattern = if matches!(
                    self.peek(),
                    None | Some(b' ' | b'\t' | b'\r' | b'\n' | b'}')
                ) {
                    None
                } else {
                    Some(self.pattern()?)
                };
                Operation::Guard {
                    pattern,
                    count,
                    invert: opcode == Some(b'v'),
                    command: self.child(depth)?,
                }
            }
            Some(b's') => {
                return Err(
                    self.error("substitution is unsupported; use x/pattern/ c/replacement/")
                );
            }
            Some(kind @ (b'!' | b'<' | b'>' | b'|')) => {
                if kind == b'!' && address.is_some() {
                    return Err(self.error("! does not accept an address"));
                }
                Operation::Shell {
                    kind,
                    command: self.shell()?,
                }
            }
            Some(b'r') => Operation::Read(self.filenames()?),
            Some(b'X' | b'Y') if self.host_commands.is_some() => {
                self.saw_file_loop = true;
                if address.is_some() {
                    return Err(self.error("file loops do not accept an address"));
                }
                let invert = opcode == Some(b'Y');
                let pattern = if matches!(self.peek(), None | Some(b' ' | b'\t' | b'\n' | b'}')) {
                    if invert {
                        return Err(self.error("Y requires a regular expression"));
                    }
                    None
                } else {
                    Some(self.pattern()?)
                };
                self.spaces();
                if matches!(self.peek(), None | Some(b'\n' | b'}')) {
                    return Err(self.error("file loop requires a child command"));
                }
                Operation::FileLoop {
                    pattern,
                    invert,
                    command: Box::new(self.command(depth + 1)?),
                }
            }
            Some(b'w' | b'e' | b'b' | b'B' | b'D' | b'X' | b'Y') => {
                return Err(SamError::new(
                    offset,
                    "this file command is not supported yet",
                ));
            }
            Some(c) => {
                return Err(SamError::new(
                    offset,
                    format!("unsupported command {:?}", char::from(c)),
                ));
            }
        };
        Ok(Command {
            address,
            operation,
            offset,
        })
    }

    fn parse(&mut self) -> Result<Vec<Command>, SamError> {
        let mut commands = Vec::new();
        self.whitespace();
        while self.peek().is_some() {
            if self.peek() == Some(b'}') {
                return Err(self.error("unbalanced command group"));
            }
            commands.push(self.command(0)?);
            self.whitespace();
        }
        Ok(commands)
    }
}

struct Edit {
    target_window: Option<usize>,
    target_selection: Option<u64>,
    range: Range<usize>,
    bytes: Vec<u8>,
    insertion: bool,
    detached: bool,
    input: usize,
}

struct Evaluator<'a> {
    data: &'a [u8],
    edits: Vec<Edit>,
    selections: Vec<Range<usize>>,
    selection_inputs: Vec<usize>,
    selection_detached: Vec<bool>,
    captures: Vec<Option<Range<usize>>>,
    capture_values: Option<&'a CaptureValues>,
    steps: usize,
    expanded_bytes: usize,
    marks: &'a BTreeMap<char, Vec<Option<usize>>>,
    input: usize,
    target_window: Option<usize>,
    target_selection: Option<u64>,
    shell: ShellContext<'a>,
}

fn line_next(data: &[u8], pos: usize) -> usize {
    data[pos..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(data.len(), |i| pos + i + 1)
}

fn line_number(data: &[u8], pos: usize) -> usize {
    1 + data[..pos].iter().filter(|&&b| b == b'\n').count()
}

fn line_position(data: &[u8], number: usize) -> Option<usize> {
    if number <= 1 {
        return Some(0);
    }
    data.iter()
        .enumerate()
        .filter(|(_, b)| **b == b'\n')
        .nth(number - 2)
        .map(|(i, _)| i + 1)
}

/// Drain both subprocess output streams even when a limit is reached, retaining
/// only the bounded prefix. This avoids pipe deadlocks and unbounded buffers.
fn capture(mut reader: impl Read, limit: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut chunk = [0u8; 8192];
    loop {
        let len = match reader.read(&mut chunk) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if len == 0 {
            break;
        }
        let retained = len.min(limit - bytes.len());
        bytes.extend_from_slice(&chunk[..retained]);
        truncated |= retained < len;
    }
    Ok((bytes, truncated))
}

/// Armed before entering the callback, since an error may follow a partial
/// terminal transfer. Explicit restoration reports errors; Drop retries a failed
/// restoration and also covers early returns after the guard has been armed.
#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
struct ProcessGroupGuard<'a> {
    observer: Option<&'a ProcessGroupObserver>,
    active: bool,
}

#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
impl<'a> ProcessGroupGuard<'a> {
    fn new(observer: Option<&'a ProcessGroupObserver>) -> Self {
        Self {
            observer,
            active: observer.is_some(),
        }
    }

    fn enter(&self, group: u32) -> io::Result<()> {
        if let Some(observer) = self.observer {
            (observer.0)(Some(group))?;
            // The child may have attempted terminal input before the host
            // transferred foreground control and stopped itself with SIGTTIN.
            use rustix::process::{Pid, Signal, kill_process_group};
            let group = i32::try_from(group)
                .ok()
                .and_then(Pid::from_raw)
                .filter(|pid| *pid != Pid::INIT)
                .ok_or_else(|| io::Error::other("invalid shell process group"))?;
            match kill_process_group(group, Signal::CONT) {
                Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn restore(&mut self) -> io::Result<()> {
        if self.active {
            (self.observer.expect("active observer").0)(None)?;
            self.active = false;
        }
        Ok(())
    }
}

#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
impl Drop for ProcessGroupGuard<'_> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Observe the leader without reaping it: its waitable PID reserves the process
/// group ID until all cleanup signals have been sent.
#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
fn shell_exited(child: &std::process::Child) -> io::Result<bool> {
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        let pid = i32::try_from(child.id())
            .ok()
            .and_then(Pid::from_raw)
            .ok_or_else(|| io::Error::other("invalid child process ID"))?;
        Ok(waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        )?
        .is_some())
    }
    #[cfg(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))]
    {
        let _ = child;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "safe shell cancellation requires waitid(NOWAIT)",
        ))
    }
}

/// Keep observing cancellation until both the leader and all three pipe tasks
/// finish: a shell can exit while a background descendant still owns a pipe.
#[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
fn wait_shell(
    child: &mut std::process::Child,
    interrupt: Option<&AtomicBool>,
    pipes_finished: impl Fn() -> bool,
) -> (io::Result<std::process::ExitStatus>, bool) {
    use rustix::process::{Pid, Signal, kill_process_group};
    use std::time::{Duration, Instant};
    const TERM_GRACE: Duration = Duration::from_millis(150);
    const CHECK_INTERVAL: Duration = Duration::from_millis(10);

    // This is our newly created process-group leader, never an inherited group.
    // Excluding INIT also avoids kill(-1, ...) if an invalid host PID appeared.
    let group = i32::try_from(child.id())
        .ok()
        .and_then(Pid::from_raw)
        .filter(|pid| *pid != Pid::INIT);
    let signal = |child: &mut std::process::Child, signal| {
        if let Some(group) = group {
            let _ = kill_process_group(group, signal);
        } else {
            let _ = child.kill();
        }
    };
    let mut exited = false;
    let mut error = None;
    let mut cancelled = false;
    let mut terminating = None;
    let mut killed = false;
    loop {
        if interrupt.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            cancelled = true;
        }
        if !exited && error.is_none() {
            match shell_exited(child) {
                Ok(found) => exited = found,
                Err(found) if found.kind() == io::ErrorKind::Interrupted => {}
                Err(found) => {
                    if found.raw_os_error() == Some(rustix::io::Errno::CHILD.raw_os_error()) {
                        // An external reaper has released this PID. Neither the
                        // PID nor PGID is ours to signal after ownership is lost.
                        return (Err(found), cancelled);
                    }
                    error = Some(found);
                }
            }
        }
        if (cancelled || error.is_some()) && terminating.is_none() {
            signal(child, Signal::TERM);
            terminating = Some(Instant::now());
        }
        if !killed && terminating.is_some_and(|start| start.elapsed() >= TERM_GRACE) {
            // Always escalate after cancellation, even if the leader exited:
            // ignored TERM or closed pipe descriptors must not spare descendants.
            signal(child, Signal::KILL);
            killed = true;
        }
        if (exited || error.is_some()) && pipes_finished() && (terminating.is_none() || killed) {
            // Even successful shells can leave descendants with closed pipes.
            // The leader remains waitable until this final group cleanup, so
            // its PID/PGID cannot be reused for an unrelated process group.
            if !killed {
                signal(child, Signal::KILL);
            }
            break;
        }
        std::thread::sleep(CHECK_INTERVAL);
    }
    let status = child.wait();
    if let Some(error) = error {
        (Err(error), cancelled)
    } else {
        (status, cancelled)
    }
}

#[cfg(unix)]
fn run_shell(
    command: &str,
    input: &[u8],
    keep_output: bool,
    context: ShellContext<'_>,
    offset: usize,
) -> Result<Vec<u8>, SamError> {
    let shell = context.shell;
    use std::io::Write;
    use std::process::{Command as ProcessCommand, Stdio};
    let io_error = |error| SamError::new(offset, format!("shell I/O: {error}"));
    if cfg!(all(
        any(feature = "terminal", feature = "lua"),
        any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )
    )) {
        return Err(SamError::new(
            offset,
            "safe shell cancellation requires waitid(NOWAIT)",
        ));
    }
    if context.interrupted() {
        return Err(SamError::new(offset, "shell command cancelled"));
    }
    let mut process = ProcessCommand::new(shell);
    process.args(["-c", command]);
    if let Some(directory) = context.directory {
        process.current_dir(directory);
    }
    #[cfg(any(feature = "terminal", feature = "lua"))]
    {
        use std::os::unix::process::CommandExt;
        process.process_group(0);
    }
    let mut child = process
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io_error)?;
    #[cfg(any(feature = "terminal", feature = "lua"))]
    let mut foreground = ProcessGroupGuard::new(context.process_group_observer);
    #[cfg(any(feature = "terminal", feature = "lua"))]
    if let Err(error) = foreground.enter(child.id()) {
        // Some may have transferred terminal control before returning an error.
        // Close our pipe ends and clean up the still-owned group before None.
        drop(child.stdin.take());
        drop(child.stdout.take());
        drop(child.stderr.take());
        let cancel = AtomicBool::new(true);
        let _ = wait_shell(&mut child, Some(&cancel), || true);
        let _ = foreground.restore();
        return Err(io_error(error));
    }
    let mut stdin = child.stdin.take().expect("piped subprocess stdin");
    let stdout = child.stdout.take().expect("piped subprocess stdout");
    let stderr = child.stderr.take().expect("piped subprocess stderr");
    let (status, output, errors, input_result, cancelled) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let reader =
            scope.spawn(move || capture(stdout, if keep_output { MAX_EXPANDED_TEXT } else { 0 }));
        let errors = scope.spawn(move || capture(stderr, 16 * 1024));
        #[cfg(any(feature = "terminal", feature = "lua"))]
        let (status, cancelled) = wait_shell(&mut child, context.interrupt, || {
            writer.is_finished() && reader.is_finished() && errors.is_finished()
        });
        #[cfg(not(any(feature = "terminal", feature = "lua")))]
        let (status, cancelled) = (child.wait(), false);
        #[cfg(not(any(feature = "terminal", feature = "lua")))]
        if status.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        (
            status,
            reader.join(),
            errors.join(),
            writer.join(),
            cancelled,
        )
    });
    #[cfg(any(feature = "terminal", feature = "lua"))]
    foreground.restore().map_err(io_error)?;
    if cancelled {
        return Err(SamError::new(offset, "shell command cancelled"));
    }
    let status = status.map_err(io_error)?;
    let (output, truncated) = output
        .map_err(|_| {
            SamError::new(offset, "shell output reader panicked")
                .in_phase(SamErrorPhase::Transcript)
        })?
        .map_err(io_error)?;
    let (errors, _) = errors
        .map_err(|_| {
            SamError::new(offset, "shell error reader panicked").in_phase(SamErrorPhase::Transcript)
        })?
        .map_err(io_error)?;
    let input_result = input_result.map_err(|_| {
        SamError::new(offset, "shell input writer panicked").in_phase(SamErrorPhase::Transcript)
    })?;
    if !status.success() {
        return Err(SamError::new(
            offset,
            format!(
                "shell command failed ({status}): {}",
                String::from_utf8_lossy(&errors).trim_end()
            ),
        ));
    }
    if let Err(error) = input_result {
        // A successful command such as head may deliberately stop reading early.
        if error.kind() != io::ErrorKind::BrokenPipe {
            return Err(io_error(error));
        }
    }
    if keep_output && truncated {
        return Err(SamError::new(offset, "shell output limit exceeded")
            .in_phase(SamErrorPhase::Transcript));
    }
    Ok(output)
}

#[cfg(not(unix))]
fn run_shell(
    _command: &str,
    _input: &[u8],
    _keep_output: bool,
    _context: ShellContext<'_>,
    offset: usize,
) -> Result<Vec<u8>, SamError> {
    Err(SamError::new(
        offset,
        "shell commands require a Unix platform",
    ))
}

impl Evaluator<'_> {
    fn tick(&mut self, offset: usize) -> Result<(), SamError> {
        if self.shell.interrupted() {
            return Err(SamError::new(offset, "command cancelled"));
        }
        self.steps += 1;
        if self.steps > MAX_STEPS {
            return Err(SamError::new(offset, "command execution limit exceeded")
                .in_phase(SamErrorPhase::Transcript));
        }
        Ok(())
    }

    fn regex(pattern: &str, offset: usize) -> Result<Regex, SamError> {
        Regex::new(pattern).map_err(|e| {
            SamError::new(offset, format!("regular expression: {e}"))
                .in_phase(SamErrorPhase::Transcript)
        })
    }

    fn find(
        &self,
        regex: &Regex,
        range: Range<usize>,
        offset: usize,
    ) -> Result<Option<Match>, SamError> {
        if range.is_empty() {
            return Ok(None);
        }
        regex.find(self.data, range).map_err(|e| {
            SamError::new(offset, format!("regular expression: {e}"))
                .in_phase(SamErrorPhase::Transcript)
        })
    }

    fn line_address(
        &self,
        number: usize,
        sign: i8,
        range: &Range<usize>,
        offset: usize,
    ) -> Result<Range<usize>, SamError> {
        let number = if sign > 0 {
            let end = if range.start < range.end && self.data.get(range.end - 1) == Some(&b'\n') {
                range.end - 1
            } else {
                range.end
            };
            line_number(self.data, end).saturating_add(number)
        } else if sign < 0 {
            line_number(self.data, range.start)
                .saturating_sub(number)
                .max(1)
        } else {
            if number == 0 {
                return Ok(0..0);
            }
            number
        };
        let start = line_position(self.data, number)
            .ok_or_else(|| SamError::new(offset, "line address is outside the document"))?;
        Ok(start..line_next(self.data, start))
    }

    fn address(
        &mut self,
        address: &Address,
        dot: Range<usize>,
        offset: usize,
    ) -> Result<Range<usize>, SamError> {
        self.tick(offset)?;
        match address {
            Address::Span {
                left,
                right,
                semicolon,
            } => {
                let left = match left {
                    Some(a) => self.address(a, dot.clone(), offset)?,
                    None => 0..0,
                };
                let right = match right {
                    Some(a) => {
                        self.address(a, if *semicolon { left.clone() } else { dot }, offset)?
                    }
                    None => self.data.len()..self.data.len(),
                };
                // The C implementation uses a union even for reversed addresses.
                Ok(left.start.min(right.start)..left.end.max(right.end))
            }
            Address::Chain(atoms) => {
                let mut range = dot;
                let mut sign = 0i8;
                for (i, atom) in atoms.iter().enumerate() {
                    self.tick(offset)?;
                    match atom {
                        Atom::Plus | Atom::Minus => {
                            sign = if matches!(atom, Atom::Plus) { 1 } else { -1 };
                            if matches!(atoms.get(i + 1), None | Some(Atom::Plus | Atom::Minus)) {
                                range = self.line_address(1, sign, &range, offset)?;
                            }
                            continue;
                        }
                        Atom::Dot => {}
                        Atom::End => range = self.data.len()..self.data.len(),
                        Atom::All => range = 0..self.data.len(),
                        Atom::Mark(name) => {
                            let pos = self
                                .marks
                                .get(name)
                                .and_then(|marks| marks.get(self.input))
                                .copied()
                                .flatten()
                                .ok_or_else(|| {
                                    SamError::new(
                                        offset,
                                        format!(
                                            "mark {name:?} is unset or deleted for input {}",
                                            self.input
                                        ),
                                    )
                                })?;
                            range = pos..pos;
                        }
                        Atom::Byte(n) => {
                            let n = if sign > 0 {
                                range.end.checked_add(*n)
                            } else if sign < 0 {
                                range.start.checked_sub(*n)
                            } else {
                                Some(*n)
                            };
                            let n = n.filter(|&n| n <= self.data.len()).ok_or_else(|| {
                                SamError::new(offset, "byte address is outside the document")
                            })?;
                            range = n..n;
                        }
                        Atom::Line(n, goto) => {
                            range = self.line_address(*n, sign, &range, offset)?;
                            if goto.get() {
                                range.end = range.start;
                            }
                        }
                        Atom::Search { pattern, backwards } => {
                            let regex = Self::regex(pattern, offset)?;
                            let backwards = if sign < 0 { !backwards } else { *backwards };
                            let found = if backwards {
                                let end = range.start;
                                let mut start = 0;
                                let mut last = None;
                                while start < end {
                                    self.tick(offset)?;
                                    let not_bol = start > 0 && self.data[start - 1] != b'\n';
                                    let Some(found) = regex
                                        .find_with_flags(self.data, start..end, not_bol, false)
                                        .map_err(|e| {
                                            SamError::new(
                                                offset,
                                                format!("regular expression: {e}"),
                                            )
                                            .in_phase(SamErrorPhase::Transcript)
                                        })?
                                    else {
                                        break;
                                    };
                                    let next = if found.range.end == start {
                                        line_next(self.data, start).min(end)
                                    } else {
                                        found.range.end
                                    };
                                    last = Some(found);
                                    if next <= start {
                                        break;
                                    }
                                    start = next;
                                }
                                last
                            } else {
                                self.find(&regex, range.end..self.data.len(), offset)?
                            };
                            range = found
                                .ok_or_else(|| {
                                    SamError::new(
                                        offset,
                                        "address regular expression did not match",
                                    )
                                })?
                                .range;
                        }
                    }
                    // Subsequent simple addresses have an implicit plus.
                    sign = 1;
                }
                Ok(range)
            }
        }
    }

    fn expand(&mut self, text: &[u8], count: usize, offset: usize) -> Result<Vec<u8>, SamError> {
        let mut bytes = Vec::new();
        let mut i = 0;
        while i < text.len() {
            let capture = if text[i] == b'&' {
                Some(0)
            } else if text[i] == b'\\' && text.get(i + 1).is_some_and(|b| (b'1'..=b'9').contains(b))
            {
                i += 1;
                Some(usize::from(text[i] - b'0'))
            } else {
                None
            };
            if let Some(capture) = capture {
                if let Some(values) = self.capture_values {
                    if let Some(Some(range)) = values.ranges.get(capture) {
                        bytes.extend_from_slice(&values.data[range.clone()]);
                    }
                } else if let Some(Some(range)) = self.captures.get(capture) {
                    bytes.extend_from_slice(&self.data[range.clone()]);
                }
            } else {
                if text[i] == b'\\' && matches!(text.get(i + 1), Some(b'\\' | b'&')) {
                    i += 1;
                }
                bytes.push(text[i]);
            }
            i += 1;
            if bytes.len() > MAX_EXPANDED_TEXT {
                return Err(SamError::new(offset, "expanded text limit exceeded")
                    .in_phase(SamErrorPhase::Transcript));
            }
        }
        let size = bytes
            .len()
            .checked_mul(count)
            .and_then(|n| n.checked_add(self.expanded_bytes))
            .filter(|&n| n <= MAX_EXPANDED_TEXT)
            .ok_or_else(|| {
                SamError::new(offset, "expanded text limit exceeded")
                    .in_phase(SamErrorPhase::Transcript)
            })?;
        self.expanded_bytes = size;
        Ok(bytes.repeat(count))
    }

    fn edit(
        &mut self,
        range: Range<usize>,
        bytes: Vec<u8>,
        insertion: bool,
        detached: bool,
        offset: usize,
    ) -> Result<(), SamError> {
        // Mirror the linked transcript insertion order, including stable order
        // for multiple insertions at one byte and insertions on range boundaries.
        let index = self
            .edits
            .iter()
            .position(|edit| edit.range.end > range.start)
            .unwrap_or(self.edits.len());
        if self
            .edits
            .get(index)
            .is_some_and(|edit| edit.range.start < range.end)
        {
            return Err(
                SamError::new(offset, "conflicting changes").in_phase(SamErrorPhase::Transcript)
            );
        }
        self.edits.insert(
            index,
            Edit {
                target_window: self.target_window,
                target_selection: self.target_selection,
                range,
                bytes,
                insertion,
                detached,
                input: self.input,
            },
        );
        Ok(())
    }

    fn extract(
        &mut self,
        pattern: Option<&str>,
        complement: bool,
        child: &Command,
        dot: &Range<usize>,
        offset: usize,
        simulate: bool,
    ) -> Result<usize, SamError> {
        let mut matches = 0;
        if let Some(pattern) = pattern {
            let regex = Self::regex(pattern, offset)?;
            let end = dot.end;
            let mut start = dot.start;
            let mut last_start = if complement { Some(start) } else { None };
            while start <= end {
                self.tick(offset)?;
                let not_bol = start > dot.start && self.data[start - 1] != b'\n';
                let found = if start < end {
                    regex
                        .find_with_flags(self.data, start..end, not_bol, false)
                        .map_err(|e| {
                            SamError::new(offset, format!("regular expression: {e}"))
                                .in_phase(SamErrorPhase::Transcript)
                        })?
                } else {
                    None
                };
                let Some(found) = found else {
                    if complement {
                        let mut range = start..end;
                        if simulate {
                            matches += 1;
                        } else {
                            self.command(child, &mut range, true)?;
                        }
                    }
                    break;
                };
                let matched = found.range.clone();
                let mut range = if complement {
                    last_start.unwrap_or(start)..matched.start
                } else {
                    matched.clone()
                };
                if matched.is_empty() {
                    if last_start == Some(matched.start) {
                        start = start.saturating_add(1);
                        continue;
                    }
                    if end == matched.start
                        && start > dot.start
                        && self.data.get(end.wrapping_sub(1)) == Some(&b'\n')
                    {
                        break;
                    }
                    start = matched.end.saturating_add(1);
                } else {
                    start = matched.end;
                }
                self.captures = found.groups;
                if self.captures.is_empty() {
                    self.captures.push(Some(matched.clone()));
                }
                last_start = Some(matched.end);
                if simulate {
                    matches += 1;
                } else {
                    self.command(child, &mut range, true)?;
                }
            }
        } else {
            let mut start = dot.start;
            while start < dot.end {
                self.tick(offset)?;
                let next = line_next(self.data, start).min(dot.end);
                if next == start {
                    break;
                }
                let mut range = start..next;
                if simulate {
                    matches += 1;
                } else {
                    self.command(child, &mut range, true)?;
                }
                start = next;
            }
        }
        Ok(matches)
    }

    fn command(
        &mut self,
        command: &Command,
        dot: &mut Range<usize>,
        detached: bool,
    ) -> Result<(), SamError> {
        self.tick(command.offset)?;
        if let Some(address) = &command.address {
            *dot = self.address(address, dot.clone(), command.offset)?;
        }
        if dot.start > dot.end || dot.end > self.data.len() {
            return Err(SamError::new(command.offset, "invalid selection range")
                .in_phase(SamErrorPhase::Validate));
        }
        self.operation(command, dot, detached)
    }

    fn operation(
        &mut self,
        command: &Command,
        dot: &mut Range<usize>,
        detached: bool,
    ) -> Result<(), SamError> {
        match &command.operation {
            Operation::Insert {
                at_end,
                text,
                count,
            } => {
                let pos = if *at_end { dot.end } else { dot.start };
                let bytes = self.expand(text, *count, command.offset)?;
                self.edit(pos..pos, bytes, true, detached, command.offset)?;
            }
            Operation::Change { text, count } => {
                let bytes = self.expand(text, *count, command.offset)?;
                self.edit(dot.clone(), bytes, true, detached, command.offset)?;
            }
            Operation::Delete => {
                self.edit(dot.clone(), Vec::new(), false, detached, command.offset)?
            }
            Operation::Select => {
                self.selections.push(dot.clone());
                self.selection_inputs.push(self.input);
                self.selection_detached.push(detached);
            }
            Operation::Shell {
                kind,
                command: shell,
            } => {
                let input = if matches!(kind, b'|' | b'>') {
                    &self.data[dot.clone()]
                } else {
                    &[]
                };
                let keep_output = matches!(kind, b'|' | b'<');
                let output = run_shell(shell, input, keep_output, self.shell, command.offset)?;
                if keep_output {
                    self.expanded_bytes = self
                        .expanded_bytes
                        .checked_add(output.len())
                        .filter(|&n| n <= MAX_EXPANDED_TEXT)
                        .ok_or_else(|| {
                            SamError::new(command.offset, "expanded text limit exceeded")
                                .in_phase(SamErrorPhase::Transcript)
                        })?;
                    self.edit(dot.clone(), output, true, detached, command.offset)?;
                }
            }
            Operation::FileLoop { .. } | Operation::Host { .. } => {
                return Err(SamError::new(command.offset, "host context required")
                    .in_phase(SamErrorPhase::Validate));
            }
            Operation::Read(names) => {
                let mut bytes = Vec::new();
                for name in names {
                    let read_error = |error| {
                        SamError::new(command.offset, format!("cannot read {name:?}: {error}"))
                    };
                    let path = self
                        .shell
                        .directory
                        .map_or_else(|| PathBuf::from(name), |directory| directory.join(name));
                    let metadata = std::fs::metadata(&path).map_err(read_error)?;
                    if !metadata.is_file() {
                        return Err(SamError::new(command.offset, "r requires a regular file"));
                    }
                    let file = std::fs::File::open(&path).map_err(read_error)?;
                    if !file.metadata().map_err(read_error)?.is_file() {
                        return Err(SamError::new(command.offset, "r requires a regular file"));
                    }
                    let (part, truncated) =
                        capture(file, MAX_EXPANDED_TEXT - self.expanded_bytes - bytes.len())
                            .map_err(read_error)?;
                    if truncated {
                        return Err(SamError::new(command.offset, "file read limit exceeded")
                            .in_phase(SamErrorPhase::Transcript));
                    }
                    bytes.extend_from_slice(&part);
                }
                self.expanded_bytes += bytes.len();
                self.edit(dot.clone(), bytes, true, detached, command.offset)?;
            }
            Operation::Group(commands) => {
                for child in commands {
                    self.command(child, dot, true)?;
                }
            }
            Operation::Guard {
                pattern,
                count,
                invert,
                command: child,
            } => {
                let in_count = count.next_matches();
                let matched = if let Some(pattern) = pattern {
                    let regex = Self::regex(pattern, command.offset)?;
                    self.find(&regex, dot.clone(), command.offset)?
                        .is_some_and(|m| m.range.start < dot.end)
                } else {
                    true
                };
                if (in_count && matched) != *invert {
                    self.command(child, dot, detached)?;
                }
            }
            Operation::Extract {
                pattern,
                complement,
                command: child,
            } => {
                let matches = if child.negative_count() {
                    self.extract(
                        pattern.as_deref(),
                        *complement,
                        child,
                        dot,
                        command.offset,
                        true,
                    )?
                } else {
                    0
                };
                child.reset_counts(Some(matches));
                self.extract(
                    pattern.as_deref(),
                    *complement,
                    child,
                    dot,
                    command.offset,
                    false,
                )?;
            }
        }
        Ok(())
    }
}

type NodePath = Vec<usize>;
type CaptureRanges = Vec<Option<Range<usize>>>;
type ExtractedMatch = (Range<usize>, Option<CaptureRanges>);
#[derive(Clone)]
struct CaptureValues {
    data: Arc<Vec<u8>>,
    ranges: Vec<Option<Range<usize>>>,
}
#[derive(Default)]
struct Transcript {
    edits: Vec<Edit>,
}
struct Returned {
    success: bool,
    dot: Range<usize>,
}
struct SelectFrame {
    path: NodePath,
    window: usize,
    inputs: Option<Vec<u64>>,
    next: usize,
    waiting: bool,
    success: bool,
}
struct CallFrame {
    path: NodePath,
    window: usize,
    file: usize,
    dot: Range<usize>,
    selection: Option<u64>,
    input: usize,
    stage: CallStage,
}
enum CallStage {
    Enter,
    Group {
        next: usize,
        waiting: bool,
        success: bool,
    },
    Child,
    Extract {
        state: ExtractState,
        waiting: bool,
        success: bool,
    },
    Files {
        windows: VecDeque<usize>,
        waiting: bool,
        success: bool,
    },
}
struct ExtractState {
    regex: Option<Regex>,
    complement: bool,
    start: usize,
    end: usize,
    original_start: usize,
    last_start: Option<usize>,
    done: bool,
}
enum Frame {
    Select(SelectFrame),
    Call(CallFrame),
}
enum Pending {
    Files {
        frame: CallFrame,
        request: FileLoopRequest,
    },
    Host {
        frame: CallFrame,
        request: HostRequest,
    },
    Selection(SelectionRequest),
}

/// Resumable structural execution. All yielded values are owned, and no Text,
/// Session or host borrow survives advance. Text changes remain queued per file
/// until the caller has validated every live transcript and commits them.
pub struct Continuation {
    program: Rc<Program>,
    stack: Vec<Frame>,
    returned: Option<Returned>,
    pending: Option<Pending>,
    events: VecDeque<SamStep>,
    transcripts: BTreeMap<usize, Transcript>,
    validated: BTreeSet<usize>,
    captures: Option<CaptureValues>,
    steps: usize,
    expanded_bytes: usize,
    failed: Option<SamError>,
    evaluation_error: Option<SamError>,
    complete: bool,
    visual: Option<bool>,
}
impl Program {
    pub fn start(self, initial_window: usize) -> Continuation {
        Continuation {
            program: Rc::new(self),
            stack: vec![Frame::Select(SelectFrame {
                path: vec![0],
                window: initial_window,
                inputs: None,
                next: 0,
                waiting: false,
                success: true,
            })],
            returned: None,
            pending: None,
            events: VecDeque::new(),
            transcripts: BTreeMap::new(),
            validated: BTreeSet::new(),
            captures: None,
            steps: 0,
            expanded_bytes: 0,
            failed: None,
            evaluation_error: None,
            complete: false,
            visual: None,
        }
    }
}
impl Continuation {
    pub fn visual(&self) -> bool {
        self.visual.unwrap_or(false)
    }
    pub fn required_window(&self) -> Option<usize> {
        match self.stack.last()? {
            Frame::Select(frame) => Some(frame.window),
            Frame::Call(frame) => Some(frame.window),
        }
    }
    fn invalid(message: &str) -> SamError {
        SamError::new(0, message).in_phase(SamErrorPhase::Validate)
    }
    pub fn resume_files(&mut self, windows: Vec<usize>) -> Result<(), SamError> {
        if windows.len() > MAX_STEPS {
            return Err(Self::invalid("file iteration limit exceeded"));
        }
        match self.pending.take() {
            Some(Pending::Files { mut frame, .. }) => {
                frame.stage = CallStage::Files {
                    windows: windows.into(),
                    waiting: false,
                    success: true,
                };
                self.stack.push(Frame::Call(frame));
                Ok(())
            }
            pending => {
                self.pending = pending;
                Err(Self::invalid("no pending file iteration"))
            }
        }
    }
    pub fn resume_host(&mut self, success: bool) -> Result<(), SamError> {
        match self.pending.take() {
            Some(Pending::Host { frame, .. }) => {
                self.finish(frame, success);
                Ok(())
            }
            pending => {
                self.pending = pending;
                Err(Self::invalid("no pending host command"))
            }
        }
    }
    pub fn resume_selection(&mut self) -> Result<(), SamError> {
        match self.pending.take() {
            Some(Pending::Selection(_)) => Ok(()),
            pending => {
                self.pending = pending;
                Err(Self::invalid("no pending selection effect"))
            }
        }
    }
    /// A previously yielded target may have been closed by a callback. Its
    /// command fails, while enclosing file/extraction iterations still continue.
    pub fn skip_window(&mut self, window: usize) -> Result<(), SamError> {
        match self.stack.pop() {
            Some(Frame::Select(frame)) if frame.window == window => {
                self.returned = Some(Returned {
                    success: false,
                    dot: 0..0,
                });
                Ok(())
            }
            Some(Frame::Call(frame)) if frame.window == window => {
                self.finish(frame, false);
                Ok(())
            }
            frame => {
                if let Some(frame) = frame {
                    self.stack.push(frame);
                }
                Err(Self::invalid("window does not match the pending command"))
            }
        }
    }
    pub fn evaluation_error(&self) -> Option<&SamError> {
        self.evaluation_error.as_ref()
    }
    pub fn pending_files(&self) -> Vec<usize> {
        self.transcripts.keys().copied().collect()
    }
    pub fn discard_file(&mut self, file: usize) {
        self.transcripts.remove(&file);
        self.validated.remove(&file);
    }
    pub fn check_write(&mut self, file: usize) -> Result<(), SamError> {
        if self
            .transcripts
            .get(&file)
            .is_some_and(|transcript| !transcript.edits.is_empty())
        {
            let error = SamError::new(0, "write conflicts with pending structural changes")
                .in_phase(SamErrorPhase::Transcript);
            self.failed = Some(error.clone());
            Err(error)
        } else {
            Ok(())
        }
    }
    pub fn validate_file(&mut self, file: usize, text: &Text) -> Result<(), SamError> {
        if let Some(error) = &self.evaluation_error {
            return Err(error.clone());
        }
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        let checked = self.transcripts.get(&file).is_none_or(|transcript| {
            let (mut removed, mut added) = (0usize, 0usize);
            for edit in &transcript.edits {
                if edit.range.start > edit.range.end || edit.range.end > text.len() {
                    return false;
                }
                let Some(start) = edit
                    .range
                    .start
                    .checked_sub(removed)
                    .and_then(|start| start.checked_add(added))
                else {
                    return false;
                };
                if start.checked_add(edit.bytes.len()).is_none() {
                    return false;
                }
                let Some(next_removed) = removed.checked_add(edit.range.len()) else {
                    return false;
                };
                let Some(next_added) = added.checked_add(edit.bytes.len()) else {
                    return false;
                };
                removed = next_removed;
                added = next_added;
            }
            text.len()
                .checked_sub(removed)
                .and_then(|len| len.checked_add(added))
                .is_some()
        });
        if !checked {
            let error = SamError::new(
                0,
                "pending changes exceed the live document or address space",
            )
            .in_phase(SamErrorPhase::Transcript);
            self.failed = Some(error.clone());
            return Err(error);
        }
        self.validated.insert(file);
        Ok(())
    }
    pub fn commit_file(
        &mut self,
        file: usize,
        text: &mut Text,
    ) -> Result<Vec<CommittedChange>, SamError> {
        if let Some(error) = &self.evaluation_error {
            return Err(error.clone());
        }
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if !self.complete || self.pending.is_some() || !self.events.is_empty() {
            return Err(Self::invalid(
                "program must finish before committing changes",
            ));
        }
        if self
            .transcripts
            .keys()
            .any(|file| !self.validated.contains(file))
        {
            return Err(Self::invalid(
                "all live transcripts must be validated before committing",
            ));
        }
        self.validate_file(file, text)?;
        let Some(transcript) = self.transcripts.remove(&file) else {
            return Ok(Vec::new());
        };
        let mut changes = Vec::with_capacity(transcript.edits.len());
        let (mut removed, mut added) = (0, 0);
        for edit in &transcript.edits {
            let start = edit.range.start - removed + added;
            changes.push(CommittedChange {
                window: edit.target_window.expect("host transcript target"),
                selection: edit.target_selection,
                change: Change {
                    source: edit.range.clone(),
                    range: start..start + edit.bytes.len(),
                    insertion: edit.insertion,
                    contains_newline: edit.bytes.contains(&b'\n'),
                    detached: edit.detached,
                    input: edit.input,
                },
            });
            removed += edit.range.len();
            added += edit.bytes.len();
        }
        if !transcript.edits.is_empty() {
            text.snapshot();
            for edit in transcript.edits.into_iter().rev() {
                text.replace(edit.range, &edit.bytes).map_err(|error| {
                    SamError::new(0, format!("text edit: {error}"))
                        .in_phase(SamErrorPhase::Transcript)
                })?;
            }
            text.snapshot();
        }
        Ok(changes)
    }
    fn finish(&mut self, frame: CallFrame, success: bool) {
        if let Some(source) = frame.selection.filter(|_| {
            matches!(
                self.program.node(&frame.path).operation,
                Operation::Group(_) | Operation::Extract { .. }
            )
        }) {
            self.events.push_back(SamStep::Selection(SelectionRequest {
                window: frame.window,
                file: frame.file,
                effect: SelectionEffect::Dispose { source },
            }));
        }
        self.returned = Some(Returned {
            success,
            dot: frame.dot,
        });
    }
    fn child_path(path: &[usize], index: usize) -> NodePath {
        let mut child = path.to_vec();
        child.push(index);
        child
    }
    fn evaluate<T>(
        &mut self,
        session: &Session,
        context: &WindowContext<'_>,
        frame: &CallFrame,
        action: impl FnOnce(&mut Evaluator<'_>) -> Result<T, SamError>,
    ) -> Result<T, SamError> {
        let data = context.text.to_vec();
        let marks = session.resolved_marks(context.text);
        let transcript = self.transcripts.entry(context.file).or_default();
        let mut evaluator = Evaluator {
            data: &data,
            edits: std::mem::take(&mut transcript.edits),
            selections: Vec::new(),
            selection_inputs: Vec::new(),
            selection_detached: Vec::new(),
            captures: Vec::new(),
            capture_values: self.captures.as_ref(),
            steps: self.steps,
            expanded_bytes: self.expanded_bytes,
            marks: &marks,
            input: frame.input,
            target_window: Some(frame.window),
            target_selection: frame.selection,
            shell: session.shell_context(),
        };
        let result = action(&mut evaluator);
        self.steps = evaluator.steps;
        self.expanded_bytes = evaluator.expanded_bytes;
        transcript.edits = evaluator.edits;
        self.validated.clear();
        result
    }
    fn fail_call(&mut self, frame: CallFrame, error: SamError) -> Result<SamStep, SamError> {
        if error.is_evaluation() {
            self.evaluation_error.get_or_insert_with(|| error.clone());
            self.finish(frame, false);
            Ok(SamStep::Diagnostic(error))
        } else {
            self.failed = Some(error.clone());
            Err(error)
        }
    }
    fn default_dot(
        command: &Command,
        context: &WindowContext<'_>,
        selection: &SelectionInput,
    ) -> Range<usize> {
        if context.visual {
            return selection.range.clone();
        }
        let pos = selection.range.start;
        if command.address.is_some() {
            return pos..pos;
        }
        let data = context.text.to_vec();
        match &command.operation {
            Operation::Extract { .. } if context.selections.len() == 1 => 0..data.len(),
            Operation::Host {
                command: HostCommand::Write | HostCommand::WriteQuit | HostCommand::User(_),
                ..
            } => 0..data.len(),
            Operation::Shell { kind: b'<', .. } => pos..pos,
            Operation::Shell { kind: b'>', .. }
            | Operation::Host {
                command: HostCommand::Substitute,
                ..
            } => {
                let start = data[..pos]
                    .iter()
                    .rposition(|&byte| byte == b'\n')
                    .map_or(0, |i| i + 1);
                start..line_next(&data, pos)
            }
            Operation::Read(_) => {
                let next = line_next(&data, pos);
                next..next
            }
            _ => pos..crate::motion::char_next(&data, pos),
        }
    }
    fn once(command: &Command) -> bool {
        matches!(
            command.operation,
            Operation::FileLoop { .. } | Operation::Shell { kind: b'!', .. }
        ) || matches!(&command.operation, Operation::Host { command, .. } if *command != HostCommand::Substitute)
    }
    /// Supply None to run control frames and discover the next needed window.
    /// Evaluation diagnostics are handled child failures; hard failures poison
    /// every pending transcript, so adapters cannot accidentally commit a subset.
    pub fn advance(
        &mut self,
        session: &Session,
        context: Option<WindowContext<'_>>,
    ) -> Result<SamStep, SamError> {
        if let Some(error) = &self.failed {
            return Err(error.clone());
        }
        if let Some(pending) = &self.pending {
            return Ok(match pending {
                Pending::Files { request, .. } => SamStep::Files(request.clone()),
                Pending::Host { request, .. } => SamStep::Host(request.clone()),
                Pending::Selection(request) => SamStep::Selection(request.clone()),
            });
        }
        let program = Rc::clone(&self.program);
        loop {
            if let Some(event) = self.events.pop_front() {
                if let SamStep::Selection(request) = &event {
                    self.pending = Some(Pending::Selection(request.clone()));
                }
                return Ok(event);
            }
            let Some(frame) = self.stack.pop() else {
                self.complete = true;
                return Ok(SamStep::Complete);
            };
            self.steps += 1;
            if self.steps > MAX_STEPS {
                let error = SamError::new(0, "command execution limit exceeded")
                    .in_phase(SamErrorPhase::Transcript);
                self.failed = Some(error.clone());
                return Err(error);
            }
            match frame {
                Frame::Select(mut frame) => {
                    if frame.waiting {
                        let returned = self.returned.take().expect("selection child result");
                        frame.success &= returned.success;
                        frame.waiting = false;
                    }
                    if !frame.success
                        || frame
                            .inputs
                            .as_ref()
                            .is_some_and(|inputs| frame.next >= inputs.len())
                    {
                        self.returned = Some(Returned {
                            success: frame.success,
                            dot: 0..0,
                        });
                        continue;
                    }
                    let Some(context) = context
                        .as_ref()
                        .filter(|context| context.window == frame.window)
                    else {
                        let window = frame.window;
                        self.stack.push(Frame::Select(frame));
                        return Ok(SamStep::NeedWindow(window));
                    };
                    if context.selections.is_empty()
                        || context.selections.iter().any(|selection| {
                            selection.range.start > selection.range.end
                                || selection.range.end > context.text.len()
                        })
                    {
                        let error = Self::invalid("invalid window selections");
                        self.failed = Some(error.clone());
                        return Err(error);
                    }
                    let command = program.node(&frame.path);
                    if frame.inputs.is_none() {
                        self.visual.get_or_insert(context.visual);
                        if context.visual {
                            command.reset_counts(Some(context.selections.len()));
                        }
                        let mut inputs: Vec<_> = context
                            .selections
                            .iter()
                            .map(|selection| selection.id)
                            .collect();
                        if Self::once(command) {
                            inputs.truncate(1);
                        }
                        frame.inputs = Some(inputs);
                    }
                    let id = frame.inputs.as_ref().unwrap()[frame.next];
                    frame.next += 1;
                    let Some((input, selection)) = context
                        .selections
                        .iter()
                        .enumerate()
                        .find(|(_, selection)| selection.id == id)
                    else {
                        self.stack.push(Frame::Select(frame));
                        continue;
                    };
                    if !context.visual
                        && context.selections.len() == 1
                        && matches!(command.operation, Operation::Select)
                    {
                        if let Some(Address::Chain(atoms)) = &command.address {
                            let line = match atoms.as_slice() {
                                [Atom::Line(_, goto)]
                                | [Atom::Plus | Atom::Minus, Atom::Line(_, goto)] => Some(goto),
                                _ => None,
                            };
                            if let Some(goto) = line {
                                goto.set(true);
                            }
                        }
                    }
                    let dot = Self::default_dot(command, context, selection);
                    let child = CallFrame {
                        path: frame.path.clone(),
                        window: frame.window,
                        file: context.file,
                        dot,
                        selection: Some(id),
                        input,
                        stage: CallStage::Enter,
                    };
                    frame.waiting = true;
                    self.stack.push(Frame::Select(frame));
                    self.stack.push(Frame::Call(child));
                }
                Frame::Call(mut frame) => {
                    let command = program.node(&frame.path);
                    match std::mem::replace(&mut frame.stage, CallStage::Enter) {
                        CallStage::Enter => {
                            let Some(context) = context
                                .as_ref()
                                .filter(|context| context.window == frame.window)
                            else {
                                let window = frame.window;
                                self.stack.push(Frame::Call(frame));
                                return Ok(SamStep::NeedWindow(window));
                            };
                            frame.file = context.file;
                            let dot = frame.dot.clone();
                            let resolved = self.evaluate(session, context, &frame, |evaluator| {
                                evaluator.tick(command.offset)?;
                                if let Some(address) = &command.address {
                                    evaluator.address(address, dot, command.offset)
                                } else {
                                    Ok(dot)
                                }
                            });
                            match resolved {
                                Ok(dot) => frame.dot = dot,
                                Err(error) => return self.fail_call(frame, error),
                            }
                            if frame.dot.start > frame.dot.end || frame.dot.end > context.text.len()
                            {
                                return self
                                    .fail_call(frame, Self::invalid("invalid selection range"));
                            }
                            match &command.operation {
                                Operation::Group(_) => {
                                    frame.stage = CallStage::Group {
                                        next: 0,
                                        waiting: false,
                                        success: true,
                                    };
                                    self.stack.push(Frame::Call(frame));
                                }
                                Operation::Guard {
                                    pattern,
                                    count,
                                    invert,
                                    ..
                                } => {
                                    let in_count = count.next_matches();
                                    let dot = frame.dot.clone();
                                    let matched =
                                        self.evaluate(session, context, &frame, |evaluator| {
                                            if let Some(pattern) = pattern {
                                                let regex =
                                                    Evaluator::regex(pattern, command.offset)?;
                                                Ok(evaluator
                                                    .find(&regex, dot.clone(), command.offset)?
                                                    .is_some_and(|found| {
                                                        found.range.start < dot.end
                                                    }))
                                            } else {
                                                Ok(true)
                                            }
                                        });
                                    let matched = match matched {
                                        Ok(matched) => matched,
                                        Err(error) => return self.fail_call(frame, error),
                                    };
                                    if (in_count && matched) != *invert {
                                        frame.stage = CallStage::Child;
                                        let child = CallFrame {
                                            path: Self::child_path(&frame.path, 0),
                                            window: frame.window,
                                            file: frame.file,
                                            dot: frame.dot.clone(),
                                            selection: frame.selection,
                                            input: frame.input,
                                            stage: CallStage::Enter,
                                        };
                                        self.stack.push(Frame::Call(frame));
                                        self.stack.push(Frame::Call(child));
                                    } else {
                                        self.finish(frame, true);
                                    }
                                }
                                Operation::Extract {
                                    pattern,
                                    complement,
                                    command: child,
                                } => {
                                    let dot = frame.dot.clone();
                                    let matches = if child.negative_count() {
                                        match self.evaluate(session, context, &frame, |evaluator| {
                                            evaluator.extract(
                                                pattern.as_deref(),
                                                *complement,
                                                child,
                                                &dot,
                                                command.offset,
                                                true,
                                            )
                                        }) {
                                            Ok(matches) => matches,
                                            Err(error) => return self.fail_call(frame, error),
                                        }
                                    } else {
                                        0
                                    };
                                    child.reset_counts(Some(matches));
                                    let regex = match pattern
                                        .as_ref()
                                        .map(|pattern| Evaluator::regex(pattern, command.offset))
                                        .transpose()
                                    {
                                        Ok(regex) => regex,
                                        Err(error) => return self.fail_call(frame, error),
                                    };
                                    frame.stage = CallStage::Extract {
                                        state: ExtractState {
                                            regex,
                                            complement: *complement,
                                            start: frame.dot.start,
                                            end: frame.dot.end,
                                            original_start: frame.dot.start,
                                            last_start: if *complement {
                                                Some(frame.dot.start)
                                            } else {
                                                None
                                            },
                                            done: false,
                                        },
                                        waiting: false,
                                        success: true,
                                    };
                                    self.stack.push(Frame::Call(frame));
                                }
                                Operation::FileLoop {
                                    pattern, invert, ..
                                } => {
                                    let request = FileLoopRequest {
                                        pattern: pattern.clone(),
                                        invert: *invert,
                                    };
                                    self.pending = Some(Pending::Files {
                                        frame,
                                        request: request.clone(),
                                    });
                                    return Ok(SamStep::Files(request));
                                }
                                Operation::Host {
                                    command: host,
                                    force,
                                    args,
                                } => {
                                    if matches!(host, HostCommand::Write | HostCommand::WriteQuit) {
                                        self.check_write(context.file)?;
                                    }
                                    let request = HostRequest {
                                        window: frame.window,
                                        file: context.file,
                                        command: host.clone(),
                                        force: *force,
                                        args: args.clone(),
                                        range: frame.dot.clone(),
                                        ranges: context
                                            .selections
                                            .iter()
                                            .map(|input| input.range.clone())
                                            .collect(),
                                        visual: context.visual,
                                        selection: frame.selection,
                                    };
                                    self.pending = Some(Pending::Host {
                                        frame,
                                        request: request.clone(),
                                    });
                                    return Ok(SamStep::Host(request));
                                }
                                Operation::Select => {
                                    self.events.push_back(SamStep::Selection(SelectionRequest {
                                        window: frame.window,
                                        file: frame.file,
                                        effect: SelectionEffect::Print {
                                            source: frame.selection,
                                            range: frame.dot.clone(),
                                        },
                                    }));
                                    self.finish(frame, true);
                                }
                                _ => {
                                    // Address already evaluated above; atomic operations use that
                                    // resolved dot without applying a relative address twice.
                                    let mut dot = frame.dot.clone();
                                    let result =
                                        self.evaluate(session, context, &frame, |evaluator| {
                                            evaluator.operation(
                                                command,
                                                &mut dot,
                                                frame.selection.is_none(),
                                            )
                                        });
                                    frame.dot = dot;
                                    match result {
                                        Ok(()) => self.finish(frame, true),
                                        Err(error) => return self.fail_call(frame, error),
                                    }
                                }
                            }
                        }
                        CallStage::Child => {
                            let returned = self.returned.take().expect("guard child result");
                            frame.dot = returned.dot;
                            self.finish(frame, returned.success);
                        }
                        CallStage::Group {
                            mut next,
                            waiting,
                            mut success,
                        } => {
                            if waiting {
                                let returned = self.returned.take().expect("group child result");
                                success &= returned.success;
                                frame.dot = returned.dot;
                            }
                            if !success || next >= command.children().len() {
                                self.finish(frame, success);
                            } else {
                                let child = CallFrame {
                                    path: Self::child_path(&frame.path, next),
                                    window: frame.window,
                                    file: frame.file,
                                    dot: frame.dot.clone(),
                                    selection: None,
                                    input: 0,
                                    stage: CallStage::Enter,
                                };
                                next += 1;
                                frame.stage = CallStage::Group {
                                    next,
                                    waiting: true,
                                    success,
                                };
                                self.stack.push(Frame::Call(frame));
                                self.stack.push(Frame::Call(child));
                            }
                        }
                        CallStage::Files {
                            mut windows,
                            waiting,
                            mut success,
                        } => {
                            if waiting {
                                success &= self.returned.take().expect("file child result").success;
                            }
                            if let Some(window) = windows.pop_front() {
                                let child = SelectFrame {
                                    path: Self::child_path(&frame.path, 0),
                                    window,
                                    inputs: None,
                                    next: 0,
                                    waiting: false,
                                    success: true,
                                };
                                frame.stage = CallStage::Files {
                                    windows,
                                    waiting: true,
                                    success,
                                };
                                self.stack.push(Frame::Call(frame));
                                self.stack.push(Frame::Select(child));
                            } else {
                                self.finish(frame, success);
                            }
                        }
                        CallStage::Extract {
                            mut state,
                            waiting,
                            mut success,
                        } => {
                            if waiting {
                                success &=
                                    self.returned.take().expect("extract child result").success;
                            }
                            if state.done {
                                self.finish(frame, success);
                                continue;
                            }
                            let Some(context) = context
                                .as_ref()
                                .filter(|context| context.window == frame.window)
                            else {
                                let window = frame.window;
                                frame.stage = CallStage::Extract {
                                    state,
                                    waiting: false,
                                    success,
                                };
                                self.stack.push(Frame::Call(frame));
                                return Ok(SamStep::NeedWindow(window));
                            };
                            let data = Arc::new(context.text.to_vec());
                            let next = self.next_extract(&mut state, &data, command.offset);
                            let next = match next {
                                Ok(next) => next,
                                Err(error) => return self.fail_call(frame, error),
                            };
                            if let Some((dot, captures)) = next {
                                if let Some(ranges) = captures {
                                    self.captures = Some(CaptureValues { data, ranges });
                                }
                                let child = CallFrame {
                                    path: Self::child_path(&frame.path, 0),
                                    window: frame.window,
                                    file: context.file,
                                    dot,
                                    selection: None,
                                    input: 0,
                                    stage: CallStage::Enter,
                                };
                                frame.stage = CallStage::Extract {
                                    state,
                                    waiting: true,
                                    success,
                                };
                                self.stack.push(Frame::Call(frame));
                                self.stack.push(Frame::Call(child));
                            } else {
                                self.finish(frame, success);
                            }
                        }
                    }
                }
            }
        }
    }
    fn next_extract(
        &mut self,
        state: &mut ExtractState,
        data: &[u8],
        offset: usize,
    ) -> Result<Option<ExtractedMatch>, SamError> {
        while !state.done {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                return Err(SamError::new(offset, "command execution limit exceeded")
                    .in_phase(SamErrorPhase::Transcript));
            }
            if state.end > data.len() {
                return Err(
                    SamError::new(offset, "extraction range is outside the live document")
                        .in_phase(SamErrorPhase::Transcript),
                );
            }
            if let Some(regex) = &state.regex {
                if state.start > state.end {
                    state.done = true;
                    return Ok(None);
                }
                let found = if state.start < state.end {
                    regex
                        .find_with_flags(
                            data,
                            state.start..state.end,
                            state.start > state.original_start && data[state.start - 1] != b'\n',
                            false,
                        )
                        .map_err(|error| {
                            SamError::new(offset, error.to_string())
                                .in_phase(SamErrorPhase::Transcript)
                        })?
                } else {
                    None
                };
                let Some(found) = found else {
                    state.done = true;
                    return Ok(if state.complement {
                        Some((state.start..state.end, None))
                    } else {
                        None
                    });
                };
                let matched = found.range.clone();
                let range = if state.complement {
                    state.last_start.unwrap_or(state.start)..matched.start
                } else {
                    matched.clone()
                };
                if matched.is_empty() {
                    if state.last_start == Some(matched.start) {
                        state.start = state.start.saturating_add(1);
                        continue;
                    }
                    if state.end == matched.start
                        && state.start > state.original_start
                        && data.get(state.end.wrapping_sub(1)) == Some(&b'\n')
                    {
                        state.done = true;
                        return Ok(None);
                    }
                    state.start = matched.end.saturating_add(1);
                } else {
                    state.start = matched.end;
                }
                state.last_start = Some(matched.end);
                let mut groups = found.groups;
                if groups.is_empty() {
                    groups.push(Some(matched));
                }
                return Ok(Some((range, Some(groups))));
            } else {
                if state.start >= state.end {
                    state.done = true;
                    return Ok(None);
                }
                let next = line_next(data, state.start).min(state.end);
                if next == state.start {
                    state.done = true;
                    return Ok(None);
                }
                let range = state.start..next;
                state.start = next;
                return Ok(Some((range, None)));
            }
        }
        Ok(None)
    }
}

/// Execute one structural script as a single undoable transaction.
///
/// Multiple commands are an implicit group. All addresses, regex matches, and
/// captures refer to the original document, including those in nested groups.
/// The caller supplies the initial selection; pass `0..text.len()` to operate
/// on the entire document. Failure leaves document contents and history intact.
pub fn execute(text: &mut Text, script: &str, dot: Range<usize>) -> Result<Execution, SamError> {
    Session::new().execute(text, script, dot)
}

fn execute_commands(
    text: &mut Text,
    commands: &[Command],
    dots: &[Range<usize>],
    marks: &BTreeMap<char, Vec<Option<usize>>>,
    shell: ShellContext<'_>,
    visual: bool,
) -> Result<Execution, SamError> {
    if dots.is_empty()
        || dots.len() > MAX_STEPS
        || dots
            .iter()
            .any(|dot| dot.start > dot.end || dot.end > text.len())
    {
        return Err(SamError::new(0, "invalid initial selection").in_phase(SamErrorPhase::Validate));
    }
    if visual {
        for command in commands {
            command.reset_counts(Some(dots.len()));
        }
    }
    let data = text.to_vec();
    let mut evaluator = Evaluator {
        data: &data,
        edits: Vec::new(),
        selections: Vec::new(),
        selection_inputs: Vec::new(),
        selection_detached: Vec::new(),
        captures: Vec::new(),
        capture_values: None,
        steps: 0,
        expanded_bytes: 0,
        marks,
        input: 0,
        target_window: None,
        target_selection: None,
        shell,
    };
    for (index, dot) in dots.iter().enumerate() {
        evaluator.input = index;
        let mut dot = dot.clone();
        for command in commands {
            if index > 0 && matches!(command.operation, Operation::Shell { kind: b'!', .. }) {
                continue;
            }
            if !visual && dot.is_empty() && command.address.is_none() {
                dot = match &command.operation {
                    Operation::Extract { .. } if dots.len() == 1 => 0..data.len(),
                    Operation::Shell { kind: b'<', .. } => dot,
                    Operation::Shell { kind: b'>', .. } => {
                        let start = data[..dot.start]
                            .iter()
                            .rposition(|&b| b == b'\n')
                            .map_or(0, |i| i + 1);
                        start..line_next(&data, dot.start)
                    }
                    Operation::Read(_) => {
                        let next = line_next(&data, dot.start);
                        next..next
                    }
                    _ => dot.start..crate::motion::char_next(&data, dot.start),
                };
            }
            evaluator.command(command, &mut dot, false)?;
        }
    }

    // C selections mark the first and last selected characters, not the
    // half-open end. Stable marks keep boundary insertions outside the selection
    // and invalidate a selected endpoint when its underlying bytes are deleted.
    let selection_marks = evaluator
        .selections
        .iter()
        .zip(evaluator.selection_inputs)
        .zip(evaluator.selection_detached)
        .filter_map(|((range, input), detached)| {
            let start = text.mark(range.start)?;
            let end = text.mark(if range.is_empty() {
                range.end
            } else {
                crate::motion::char_prev(&data, range.end)
            })?;
            Some((start, end, range.is_empty(), input, detached))
        })
        .collect::<Vec<_>>();
    let mut changes = Vec::with_capacity(evaluator.edits.len());
    let mut removed = 0;
    let mut added = 0;
    for edit in &evaluator.edits {
        let start = edit.range.start - removed + added;
        changes.push(Change {
            source: edit.range.clone(),
            range: start..start + edit.bytes.len(),
            insertion: edit.insertion,
            contains_newline: edit.bytes.contains(&b'\n'),
            detached: edit.detached,
            input: edit.input,
        });
        removed += edit.range.len();
        added += edit.bytes.len();
    }
    if shell.interrupted() {
        return Err(SamError::new(0, "command cancelled"));
    }
    if !evaluator.edits.is_empty() {
        text.snapshot();
        // Reverse application preserves original offsets and insertion order.
        for edit in evaluator.edits.into_iter().rev() {
            text.replace(edit.range, &edit.bytes).map_err(|e| {
                SamError::new(0, format!("text edit: {e}")).in_phase(SamErrorPhase::Transcript)
            })?;
        }
        text.snapshot();
    }
    let final_data = if selection_marks.is_empty() {
        Vec::new()
    } else {
        text.to_vec()
    };
    let mut selections = Vec::with_capacity(selection_marks.len());
    let mut selection_inputs = Vec::with_capacity(selection_marks.len());
    let mut selection_detached = Vec::with_capacity(selection_marks.len());
    for (start, end, empty, input, detached) in selection_marks {
        let (Some(start), Some(end)) = (text.mark_get(start), text.mark_get(end)) else {
            continue;
        };
        selections.push(if empty {
            start..start
        } else {
            start..crate::motion::char_next(&final_data, end)
        });
        selection_inputs.push(input);
        selection_detached.push(detached);
    }
    Ok(Execution {
        selections,
        selection_inputs,
        selection_detached,
        changes,
        output: Vec::new(),
    })
}

#[cfg(test)]
#[allow(clippy::single_range_in_vec_init)] // Commands and requests deliberately contain byte ranges.
mod tests {
    use super::*;

    fn run(input: &[u8], script: &str) -> Vec<u8> {
        let mut text = Text::from_bytes(input);
        let len = text.len();
        execute(&mut text, script, 0..len).unwrap();
        text.to_vec()
    }

    #[test]
    fn grouped_edits_use_original_coordinates() {
        assert_eq!(
            run(b"one two one\n", ",x/one/ { i/</ a/>/ }"),
            b"<one> two <one>\n"
        );
        assert_eq!(
            run(
                b"Emacs vi\n",
                ",x/(Emacs|vi)/ { g/Emacs/ v/....../ c/vi/ g/vi/ v/.../ c/Emacs/ }"
            ),
            b"vi Emacs\n"
        );
    }

    #[test]
    fn conflict_and_late_parse_errors_are_atomic() {
        for script in [
            ",{c/first/ c/second/}",
            "i/first/ {",
            "i/first/ ! false",
            "i/first/ #999d",
        ] {
            let mut text = Text::from_bytes(b"original");
            assert!(execute(&mut text, script, 0..8).is_err(), "{script}");
            assert_eq!(text.to_vec(), b"original");
            assert_eq!(text.undo(), None);
        }
    }

    struct MachineWindow {
        file: usize,
        path: Vec<u8>,
        inputs: Vec<SelectionInput>,
    }
    struct MachineHost {
        texts: BTreeMap<usize, Text>,
        windows: BTreeMap<usize, MachineWindow>,
        order: Vec<usize>,
        hosts: Vec<HostRequest>,
        effects: Vec<SelectionRequest>,
        diagnostics: Vec<SamError>,
    }
    impl MachineHost {
        fn new(files: &[(usize, &[u8])], windows: &[(usize, usize, &str, usize)]) -> Self {
            Self {
                texts: files
                    .iter()
                    .map(|(id, bytes)| (*id, Text::from_bytes(bytes)))
                    .collect(),
                windows: windows
                    .iter()
                    .map(|(window, file, path, cursor)| {
                        (
                            *window,
                            MachineWindow {
                                file: *file,
                                path: path.as_bytes().to_vec(),
                                inputs: vec![SelectionInput {
                                    id: *window as u64 + 1,
                                    range: *cursor..*cursor,
                                }],
                            },
                        )
                    })
                    .collect(),
                order: windows.iter().map(|(window, ..)| *window).collect(),
                hosts: Vec::new(),
                effects: Vec::new(),
                diagnostics: Vec::new(),
            }
        }
        fn run(
            &mut self,
            session: &Session,
            machine: &mut Continuation,
            mut callback: impl FnMut(&mut Self, &HostRequest) -> bool,
        ) -> Result<(), SamError> {
            let mut needed = None;
            for _ in 0..10_000 {
                let context = needed.and_then(|window| {
                    self.windows.get(&window).map(|view| WindowContext {
                        window,
                        file: view.file,
                        text: &self.texts[&view.file],
                        selections: &view.inputs,
                        visual: false,
                    })
                });
                let step = machine.advance(session, context)?;
                needed = None;
                match step {
                    SamStep::NeedWindow(window) => {
                        if self.windows.contains_key(&window) {
                            needed = Some(window);
                        } else {
                            machine.skip_window(window)?;
                        }
                    }
                    SamStep::Files(request) => {
                        let mut windows = Vec::new();
                        for window in &self.order {
                            if let Some(view) = self.windows.get(window) {
                                if request.matches(&view.path)? {
                                    windows.push(*window);
                                }
                            }
                        }
                        machine.resume_files(windows)?;
                    }
                    SamStep::Host(request) => {
                        self.hosts.push(request.clone());
                        let success = callback(self, &request);
                        machine.resume_host(success)?;
                    }
                    SamStep::Selection(request) => {
                        self.effects.push(request);
                        machine.resume_selection()?;
                    }
                    SamStep::Diagnostic(error) => self.diagnostics.push(error),
                    SamStep::Complete => return Ok(()),
                }
            }
            panic!("continuation did not finish within its bounded test workload");
        }
        fn commit(&mut self, machine: &mut Continuation) -> Result<Vec<CommittedChange>, SamError> {
            let files = machine.pending_files();
            for file in &files {
                machine.validate_file(*file, &self.texts[file])?;
            }
            let mut changes = Vec::new();
            for file in files {
                changes.extend(machine.commit_file(file, self.texts.get_mut(&file).unwrap())?);
            }
            Ok(changes)
        }
    }

    #[test]
    fn program_parsing_validates_nested_destructive_commands_without_effects() {
        let mut session = Session::new();
        let program = session.parse_program("X/a/ g/./ X q!", &[]).unwrap();
        assert!(program.has_file_loops());
        assert!(program.requires_host());
        for script in ["X { q }", ",x/./ X q", "g/./ { Y/b/ e file }"] {
            assert_eq!(
                session.parse_program(script, &[]).unwrap_err().phase(),
                SamErrorPhase::Validate,
                "{script}"
            );
        }
        for script in ["Y q", "X", "#0X q", "X/[/ q", "X unknown-command"] {
            assert_eq!(
                session.parse_program(script, &[]).unwrap_err().phase(),
                SamErrorPhase::Parse,
                "{script}"
            );
        }
        assert_eq!(session.previous_regex(), Some("b")); // invalid patterns do not replace the prior valid register
    }

    #[test]
    fn program_single_root_invariant_rejects_trailing_commands_and_runs_every_group_child() {
        let mut session = Session::new();
        assert_eq!(
            session
                .parse_program("X i/first/\n#0i/last/", &[])
                .unwrap_err()
                .phase(),
            SamErrorPhase::Parse
        );
        let program = session
            .parse_program("{ X i/first/\n after\n #0i/last/ }", &["after".into()])
            .unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"abc")], &[(0, 0, "a", 0)]);
        host.run(&session, &mut machine, |host, request| {
            assert_eq!(request.command, HostCommand::User("after".into()));
            assert_eq!(host.texts[&0].to_vec(), b"abc");
            true
        })
        .unwrap();
        host.commit(&mut machine).unwrap();
        assert_eq!(host.texts[&0].to_vec(), b"firstlastabc");
        assert_eq!(host.hosts.len(), 1);
    }

    #[test]
    fn workspace_program_probe_preserves_unowned_registers_and_retains_file_loop_parse_effects() {
        let mut session = Session::new();
        session.set_previous_regex("seed").unwrap();
        session.parse_program("! printf seed", &[]).unwrap();
        assert!(
            session
                .parse_workspace_program(",x/new/ p", &[])
                .unwrap()
                .is_none()
        );
        assert!(
            session
                .parse_workspace_program("not-a-command", &[])
                .unwrap()
                .is_none()
        );
        assert_eq!(session.previous_regex(), Some("seed"));
        assert_eq!(session.previous_shell(), Some("printf seed"));
        assert_eq!(
            session
                .parse_workspace_program("X/new/ ! printf updated\n{", &[])
                .unwrap_err()
                .phase(),
            SamErrorPhase::Parse
        );
        assert_eq!(session.previous_regex(), Some("new"));
        assert_eq!(session.previous_shell(), Some("printf updated"));
        assert_eq!(
            session
                .parse_workspace_program("X/later/ { q }", &[])
                .unwrap_err()
                .phase(),
            SamErrorPhase::Validate
        );
        assert_eq!(session.previous_regex(), Some("later"));
    }

    #[test]
    fn workspace_user_probe_preserves_raw_spelling_ranges_force_and_arguments() {
        let external = vec!["capture".to_owned()];
        for (script, raw, range, force, args) in [
            ("capture", "capture", 0..6, false, vec![]),
            ("cap! 'two words'", "cap", 0..6, true, vec!["two words"]),
            ("#1,#3capture! exact", "capture", 1..3, true, vec!["exact"]),
        ] {
            let mut session = Session::new();
            let program = session
                .parse_workspace_program(script, &external)
                .unwrap()
                .expect("registered User must activate workspace execution");
            assert!(!program.has_file_loops());
            assert!(program.requires_host());
            let mut machine = program.start(0);
            let mut host = MachineHost::new(&[(0, b"abcdef")], &[(0, 0, "a", 4)]);
            host.run(&session, &mut machine, |_, request| {
                assert_eq!(request.command, HostCommand::User(raw.to_owned()));
                assert_eq!(request.range, range);
                assert_eq!(request.force, force);
                assert_eq!(request.args, args);
                assert_eq!(request.selection, Some(1));
                raw == "capture" // C's abbreviated user-handler lookup fails.
            })
            .unwrap();
            assert_eq!(host.hosts.len(), 1);
            assert!(host.diagnostics.is_empty());
            assert!(machine.evaluation_error().is_none());
        }
        let mut overridden = Session::new();
        let program = overridden
            .parse_workspace_program("hel", &["help".into()])
            .unwrap()
            .unwrap();
        assert!(
            matches!(&program.commands[0].operation, Operation::Host { command: HostCommand::User(raw), .. } if raw == "hel")
        );
        let mut session = Session::new();
        let program = session
            .parse_workspace_program("capture", &external)
            .unwrap()
            .unwrap();
        let mut machine = program.start(0);
        let text = Text::from_bytes(b"abcdef");
        let selections = [
            SelectionInput {
                id: 10,
                range: 1..3,
            },
            SelectionInput {
                id: 11,
                range: 4..5,
            },
        ];
        assert!(matches!(
            machine.advance(&session, None).unwrap(),
            SamStep::NeedWindow(0)
        ));
        let SamStep::Host(request) = machine
            .advance(
                &session,
                Some(WindowContext {
                    window: 0,
                    file: 0,
                    text: &text,
                    selections: &selections,
                    visual: true,
                }),
            )
            .unwrap()
        else {
            panic!("visual User should yield one host request");
        };
        assert_eq!(request.range, 1..3);
        assert_eq!(request.ranges, vec![1..3, 4..5]);
        assert_eq!(request.selection, Some(10));
        assert!(request.visual);
        machine.resume_host(false).unwrap();
        assert!(matches!(
            machine.advance(&session, None).unwrap(),
            SamStep::Complete
        ));
    }

    #[test]
    fn workspace_user_probe_commits_recognized_parse_effects_without_affecting_editor_owned_commands()
     {
        let mut session = Session::new();
        session.set_previous_regex("seed").unwrap();
        let external = vec!["capture".to_owned()];
        assert_eq!(
            session
                .parse_workspace_program("g/new/ cap 'unterminated", &external)
                .unwrap_err()
                .phase(),
            SamErrorPhase::Parse
        );
        assert_eq!(session.previous_regex(), Some("new"));
        assert!(
            session
                .parse_workspace_program("g/ordinary/ p", &external)
                .unwrap()
                .is_none()
        );
        assert_eq!(session.previous_regex(), Some("new"));
        let nested = session
            .parse_workspace_program("g/./ { capture\n }", &external)
            .unwrap()
            .unwrap();
        assert!(nested.requires_host());
        assert!(!nested.has_file_loops());
    }

    #[test]
    fn continuation_reads_live_callback_text_and_yields_typed_arguments_and_selection_sources() {
        let mut session = Session::new();
        let program = session
            .parse_program(
                "X { mutate! 'two words' \"other path\"\n ,x/new/ c/changed/ }",
                &["mutate".into()],
            )
            .unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"old")], &[(0, 0, "a", 1)]);
        host.run(&session, &mut machine, |host, request| {
            assert_eq!(request.command, HostCommand::User("mutate".into()));
            assert!(request.force);
            assert_eq!(request.args, ["two words", "other path"]);
            assert_eq!(request.selection, None);
            host.texts
                .get_mut(&request.file)
                .unwrap()
                .replace(0..3, b"new")
                .unwrap();
            true
        })
        .unwrap();
        host.commit(&mut machine).unwrap();
        assert_eq!(host.texts[&0].to_vec(), b"changed");
        let program = session.parse_program("X { #0,#2p #2,#3p }", &[]).unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"abc")], &[(0, 0, "a", 0)]);
        host.run(&session, &mut machine, |_, _| true).unwrap();
        assert!(
            matches!(&host.effects[0].effect, SelectionEffect::Print{source:None,range} if *range == (0..2))
        );
        assert!(
            matches!(&host.effects[1].effect, SelectionEffect::Print{source:None,range} if *range == (2..3))
        );
        assert!(matches!(
            &host.effects[2].effect,
            SelectionEffect::Dispose { source: 1 }
        ));
    }

    #[test]
    fn file_loop_select_wrapper_turns_single_line_print_into_goto_motion() {
        for (script, cursor, expected) in [
            ("X 2p", 0, 2..2),
            ("X +1p", 0, 2..2),
            ("X -1p", 4, 2..2),
            ("X 2,3p", 0, 2..6),
        ] {
            let mut session = Session::new();
            let program = session.parse_program(script, &[]).unwrap();
            let mut machine = program.start(0);
            let mut host = MachineHost::new(&[(0, b"a\nb\nc\n")], &[(0, 0, "a", cursor)]);
            host.run(&session, &mut machine, |_, _| true).unwrap();
            assert!(
                matches!(&host.effects[0].effect,SelectionEffect::Print{source:Some(1),range} if *range == expected),
                "{script}"
            );
        }
    }

    #[test]
    fn continuation_structural_cases_match_the_strict_driver() {
        for (bytes, script) in [
            (&b"a\nb\n"[..], ",x/^/ i/#/"),
            (&b"baac"[..], ",x/a*/ c/[]/"),
            (&b"baac"[..], ",y/a*/ c/[]/"),
            (&b"ab"[..], ",x/(a|b)/ { i/</ a/[&]/ }"),
            (&b"a\nbb\n"[..], ",x g-1 c/X/"),
            (&b"abc\ndefgh\n"[..], ",x x/./ g-1 c/X/"),
            (&b"abc"[..], ",g/ab/ { #1i/A/ #2a/B/ }"),
        ] {
            let expected = run(bytes, script);
            let mut session = Session::new();
            let program = session.parse_program(script, &[]).unwrap();
            let mut machine = program.start(0);
            let mut host = MachineHost::new(&[(0, bytes)], &[(0, 0, "a", 0)]);
            host.run(&session, &mut machine, |_, _| {
                panic!("pure structural program yielded a host command")
            })
            .unwrap();
            host.commit(&mut machine).unwrap();
            assert_eq!(host.texts[&0].to_vec(), expected, "{script}");
        }
    }

    #[test]
    fn nested_file_loops_resume_inside_extract_guard_and_group_without_reparse() {
        let mut session = Session::new();
        let program = session
            .parse_program(
                "X/a[.]txt/ ,x/(a)/ g/a/ { i/</ Y/b[.]txt/ capture\n a/[&]/ }",
                &["capture".into()],
            )
            .unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(
            &[(0, b"a-a"), (1, b"bee"), (2, b"cat")],
            &[
                (0, 0, "/a.txt", 1),
                (1, 1, "/b.txt", 1),
                (2, 2, "/c.txt", 1),
            ],
        );
        host.run(&session, &mut machine, |_, request| {
            assert_eq!(request.command, HostCommand::User("capture".into()));
            true
        })
        .unwrap();
        assert_eq!(
            host.hosts
                .iter()
                .map(|request| request.window)
                .collect::<Vec<_>>(),
            vec![0, 2, 0, 2]
        );
        assert!(host.hosts.iter().all(|request| request.range == (0..3)));
        assert_eq!(host.texts[&0].to_vec(), b"a-a"); // delayed until the complete global transcript
        let changes = host.commit(&mut machine).unwrap();
        assert_eq!(host.texts[&0].to_vec(), b"<a[a]-<a[a]");
        assert_eq!(host.texts[&1].to_vec(), b"bee");
        assert_eq!(host.texts[&2].to_vec(), b"cat");
        assert!(
            changes
                .iter()
                .all(|change| change.window == 0 && change.selection.is_none())
        );
    }

    #[test]
    fn file_loop_uses_each_window_selection_and_keeps_shared_buffer_conflicts_atomic() {
        let mut session = Session::new();
        let program = session.parse_program("X/a/ c/Z/", &[]).unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"abc")], &[(0, 0, "a", 0), (1, 0, "a", 2)]);
        host.run(&session, &mut machine, |_, _| true).unwrap();
        let changes = host.commit(&mut machine).unwrap();
        assert_eq!(host.texts[&0].to_vec(), b"ZbZ");
        assert_eq!(
            changes
                .iter()
                .map(|change| change.selection)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
        let program = session.parse_program("X/a/ ,c/Z/", &[]).unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"abc")], &[(0, 0, "a", 0), (1, 0, "a", 2)]);
        assert_eq!(
            host.run(&session, &mut machine, |_, _| true)
                .unwrap_err()
                .phase(),
            SamErrorPhase::Transcript
        );
        assert!(host.commit(&mut machine).is_err());
        assert_eq!(host.texts[&0].to_vec(), b"abc");
        assert_eq!(host.texts.get_mut(&0).unwrap().undo(), None);
    }

    #[test]
    fn host_false_short_circuits_group_without_poisoning_text_and_closed_windows_unwind() {
        let mut session = Session::new();
        let program = session
            .parse_program("X { i/queued/ reject a/after/ }", &["reject".into()])
            .unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"a"), (1, b"b")], &[(0, 0, "a", 0), (1, 1, "b", 0)]);
        host.run(&session, &mut machine, |_, _| false).unwrap();
        assert!(host.diagnostics.is_empty());
        assert!(machine.evaluation_error().is_none());
        host.commit(&mut machine).unwrap();
        assert_eq!(host.texts[&0].to_vec(), b"queueda");
        assert_eq!(host.texts[&1].to_vec(), b"queuedb");
        let program = session.parse_program("g/./ X q!", &[]).unwrap();
        let mut machine = program.start(0);
        host.run(&session, &mut machine, |host, request| {
            host.windows.remove(&request.window);
            true
        })
        .unwrap();
        assert!(host.windows.is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn structural_evaluation_failure_poisoning_prevents_every_crossfile_commit() {
        for failing in ["r /dev/null/missing", "| false"] {
            let mut session = Session::new();
            let script = format!("X {{ i/queued/ touch\n g/b/ {failing}\n }}");
            let program = session.parse_program(&script, &["touch".into()]).unwrap();
            let mut machine = program.start(0);
            let mut host =
                MachineHost::new(&[(0, b"a"), (1, b"b")], &[(0, 0, "a", 0), (1, 1, "b", 0)]);
            host.run(&session, &mut machine, |host, request| {
                let text = host.texts.get_mut(&request.file).unwrap();
                let end = text.len();
                text.replace(end..end, b"!").unwrap();
                true
            })
            .unwrap();
            assert_eq!(host.diagnostics.len(), 1);
            assert!(host.diagnostics[0].is_evaluation());
            for file in [0, 1] {
                assert!(
                    machine
                        .validate_file(file, &host.texts[&file])
                        .unwrap_err()
                        .is_evaluation()
                );
                assert!(
                    machine
                        .commit_file(file, host.texts.get_mut(&file).unwrap())
                        .unwrap_err()
                        .is_evaluation()
                );
            }
            assert_eq!(host.texts[&0].to_vec(), b"a!");
            assert_eq!(host.texts[&1].to_vec(), b"b!");
        }
    }

    #[test]
    fn crossfile_preflight_and_writes_reject_partial_transcript_commits() {
        let mut session = Session::new();
        let program = session.parse_program("X ,c/Z/", &[]).unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(
            &[(0, b"abc"), (1, b"def")],
            &[(0, 0, "a", 0), (1, 1, "b", 0)],
        );
        host.run(&session, &mut machine, |_, _| true).unwrap();
        machine.validate_file(0, &host.texts[&0]).unwrap();
        assert!(
            machine
                .commit_file(0, host.texts.get_mut(&0).unwrap())
                .is_err()
        );
        host.texts.get_mut(&1).unwrap().replace(0..3, b"").unwrap();
        assert_eq!(
            machine
                .validate_file(1, &host.texts[&1])
                .unwrap_err()
                .phase(),
            SamErrorPhase::Transcript
        );
        assert!(
            machine
                .commit_file(0, host.texts.get_mut(&0).unwrap())
                .is_err()
        );
        assert_eq!(host.texts[&0].to_vec(), b"abc");
        let program = session.parse_program("X { i/queued/ w }", &[]).unwrap();
        let mut machine = program.start(0);
        let mut host = MachineHost::new(&[(0, b"a")], &[(0, 0, "a", 0)]);
        assert_eq!(
            host.run(&session, &mut machine, |_, _| panic!(
                "conflicting write must not reach host"
            ))
            .unwrap_err()
            .phase(),
            SamErrorPhase::Transcript
        );
        assert!(host.hosts.is_empty());
        assert_eq!(host.texts[&0].to_vec(), b"a");
    }

    #[test]
    fn error_phases_preserve_strict_batch_rollback() {
        for (script, phase) in [
            ("0i/queued/\n{", SamErrorPhase::Parse),
            ("0i/queued/\nx/[/ p", SamErrorPhase::Parse),
            ("not-a-command", SamErrorPhase::Parse),
            ("{c/first/ c/second/}", SamErrorPhase::Transcript),
            ("0i67108865/x/", SamErrorPhase::Transcript),
            ("0i/queued/\n'ap", SamErrorPhase::Evaluation),
            ("0i/queued/\n#999p", SamErrorPhase::Evaluation),
            ("0i/queued/\n/not-present/p", SamErrorPhase::Evaluation),
        ] {
            let mut session = Session::new();
            let mut text = Text::from_bytes(b"original");
            let error = session
                .execute_many(&mut text, script, &[0..8])
                .unwrap_err();
            assert_eq!(error.phase(), phase, "{script}: {error}");
            assert_eq!(error.is_evaluation(), phase == SamErrorPhase::Evaluation);
            assert_eq!(text.to_vec(), b"original", "{script}");
            assert_eq!(text.undo(), None, "{script}");
        }
    }

    #[test]
    fn error_phases_cover_validation_and_write_address_resolution() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"original");
        assert_eq!(
            session
                .execute_many(&mut text, "p", &[0..9])
                .unwrap_err()
                .phase(),
            SamErrorPhase::Validate
        );
        assert_eq!(
            session
                .resolve_write(&text, "w", &[], false)
                .unwrap_err()
                .phase(),
            SamErrorPhase::Validate
        );
        assert_eq!(
            session
                .resolve_write(&text, "'aw", &[0..0], false)
                .unwrap_err()
                .phase(),
            SamErrorPhase::Evaluation
        );
        assert_eq!(
            session
                .resolve_write(&text, "/[/w", &[0..0], false)
                .unwrap_err()
                .phase(),
            SamErrorPhase::Parse
        );
        assert_eq!(
            session.set_previous_regex("[").unwrap_err().phase(),
            SamErrorPhase::Parse
        );
        assert_eq!(text.to_vec(), b"original");
        assert_eq!(text.undo(), None);
    }

    #[test]
    #[cfg(unix)]
    fn error_phases_keep_shell_and_read_failures_strict_but_handled() {
        for script in [
            "0i/queued/\n1| false\n",
            "0i/queued/\nr /dev/null/vis-missing\n",
        ] {
            let mut session = Session::new();
            let mut text = Text::from_bytes(b"original");
            let error = session
                .execute_many(&mut text, script, &[0..8])
                .unwrap_err();
            assert!(error.is_evaluation(), "{script}: {error}");
            assert_eq!(text.to_vec(), b"original");
            assert_eq!(text.undo(), None);
        }
    }

    #[test]
    fn line_and_byte_addresses() {
        assert_eq!(
            run(b"first\nsecond\nthird\n", "2c/new\\n/"),
            b"first\nnew\nthird\n"
        );
        assert_eq!(run(b"abcdef", "#2,#4c/XY/"), b"abXYef");
        assert_eq!(run(b"a\nb\nc\n", "1+1c/B\\n/"), b"a\nB\nc\n");
        assert_eq!(run(b"a\nb\nc\n", "$-1c/C\\n/"), b"a\nb\nC\n");
        assert_eq!(run(b"abcdef", "#4-#2i/!/"), b"ab!cdef");
    }

    #[test]
    fn multiline_text_and_repetition() {
        assert_eq!(run(b"end", "0i\nhello\nworld\n.\n"), b"hello\nworld\nend");
        assert_eq!(run(b"end", "0i3/./"), b"...end");
    }

    #[test]
    fn empty_matches_terminate_and_follow_vis_eof_rule() {
        assert_eq!(run(b"a\nb\n", ",x/^/i/#/"), b"#a\n#b\n");
        assert_eq!(
            run(b"svissvis", ",x/[^s]*/ {i/[/ a/]/}"),
            b"[]s[vi]s[]s[vi]s"
        );
        assert_eq!(run(b"a b", ",y/ / { i/[/ a/]/ }"), b"[a] [b]");
    }

    #[test]
    fn captures_and_literal_escapes() {
        assert_eq!(
            run(b"ab cd", ",x/([a-z])([a-z])/c/\\2\\1(&)/"),
            b"ba(ab) dc(cd)"
        );
        assert_eq!(run(b"x", ",c/\\&\\\\/"), b"&\\");
        assert_eq!(run("♪♪\n".as_bytes(), ",x/♪/c/♫/"), "♫♫\n".as_bytes());
    }

    #[test]
    fn previous_regex_and_semicolon_dot() {
        assert_eq!(run(b"ab ab ab", "0/ab/+//c/X/"), b"ab X ab");
        assert_eq!(run(b"a\nb\nc\n", "1; +1c/X/"), b"Xc\n");
    }

    #[test]
    fn one_undo_restores_entire_transcript() {
        let mut text = Text::from_bytes(b"one two one");
        execute(&mut text, ",x/one/c/1/", 0..11).unwrap();
        assert_eq!(text.to_vec(), b"1 two 1");
        assert!(text.undo().is_some());
        assert_eq!(text.to_vec(), b"one two one");
        assert!(text.redo().is_some());
        assert_eq!(text.to_vec(), b"1 two 1");
    }

    #[test]
    fn selections_and_binary_bytes() {
        let mut text = Text::from_bytes(b"a\0b\xff");
        let result = execute(&mut text, "#1,#3p", 0..4).unwrap();
        assert_eq!(result.selections, vec![1..3]);
        assert!(result.output.is_empty());
        assert_eq!(run(b"a\0b\xff", "#1,#3c/X/"), b"aX\xff");
        // Addresses deliberately permit splitting a UTF-8 codepoint: sam.c
        // evaluates #n as a byte count, not a Unicode character count.
        assert_eq!(run("♪ab".as_bytes(), "#1,#3c/X/"), b"\xe2Xab");
        assert_eq!(run("♪ab".as_bytes(), "#3-#2,#3c/X/"), b"\xe2Xab");
    }

    #[test]
    fn transcript_reports_every_final_inserted_span() {
        let mut text = Text::from_bytes(b"one two one\n");
        let execution = execute(&mut text, ",x/one/ { i/</ a/>/ }", 0..12).unwrap();
        assert_eq!(
            execution.changes,
            vec![
                Change {
                    source: 0..0,
                    range: 0..1,
                    insertion: true,
                    contains_newline: false,
                    detached: true,
                    input: 0
                },
                Change {
                    source: 3..3,
                    range: 4..5,
                    insertion: true,
                    contains_newline: false,
                    detached: true,
                    input: 0
                },
                Change {
                    source: 8..8,
                    range: 10..11,
                    insertion: true,
                    contains_newline: false,
                    detached: true,
                    input: 0
                },
                Change {
                    source: 11..11,
                    range: 14..15,
                    insertion: true,
                    contains_newline: false,
                    detached: true,
                    input: 0
                },
            ]
        );
        assert!(execution.selections.is_empty());
        for change in execution.changes {
            assert_eq!(text.bytes(change.range).unwrap().len(), 1);
        }
    }

    #[test]
    fn transcript_distinguishes_delete_empty_change_and_newline() {
        let mut text = Text::from_bytes(b"abcdef");
        let execution = execute(&mut text, "#1,#3c/X\\n/ #3,#4d #5c//", 0..6).unwrap();
        assert_eq!(text.to_vec(), b"aX\nef");
        assert_eq!(
            execution.changes,
            vec![
                Change {
                    source: 1..3,
                    range: 1..3,
                    insertion: true,
                    contains_newline: true,
                    detached: false,
                    input: 0
                },
                Change {
                    source: 3..4,
                    range: 3..3,
                    insertion: false,
                    contains_newline: false,
                    detached: false,
                    input: 0
                },
                Change {
                    source: 5..5,
                    range: 4..4,
                    insertion: true,
                    contains_newline: false,
                    detached: false,
                    input: 0
                },
            ]
        );
    }

    #[test]
    fn extract_default_selects_all_matches_and_complement_ranges() {
        let mut text = Text::from_bytes(b"one two one");
        assert_eq!(
            execute(&mut text, ",x/one/", 0..11).unwrap().selections,
            vec![0..3, 8..11]
        );
        assert_eq!(
            execute(&mut text, ",y/ /", 0..11).unwrap().selections,
            vec![0..3, 4..7, 8..11]
        );
    }

    #[test]
    fn result_selection_context_matches_c_group_and_extract_calls() {
        for (script, expected) in [
            ("g/one/i/</", vec![false]),
            ("{ g/one/i/</ a/>/ }", vec![true, true]),
            (",x/one/g/one/i/</", vec![true, true]),
        ] {
            let mut text = Text::from_bytes(b"one two one");
            let execution = execute(&mut text, script, 0..11).unwrap();
            assert_eq!(
                execution
                    .changes
                    .iter()
                    .map(|change| change.detached)
                    .collect::<Vec<_>>(),
                expected,
                "{script}"
            );
        }
    }

    #[test]
    fn guard_counts_modulo_intervals_negative_and_regex_inversion() {
        let input = b"a\nb\nc\nd\n";
        for (script, expected) in [
            (",x { g%2 i/+/ v%2 i/-/ }", b"-a\n+b\n-c\n+d\n".as_slice()),
            (",x g2,3 c/X\\n/", b"a\nX\nX\nd\n".as_slice()),
            (",x g-1 c/Z\\n/", b"a\nb\nc\nZ\n".as_slice()),
            (",x v-1 c/Z\\n/", b"Z\nZ\nZ\nd\n".as_slice()),
            (",x v2/b/ c/X\\n/", b"X\nb\nX\nX\n".as_slice()),
        ] {
            assert_eq!(run(input, script), expected, "{script}");
        }
        assert_eq!(run(b"abc\ndef\n", ",x x/./ g2 c/X/"), b"aXc\ndXf\n");
        // C count_init includes a nested extraction at the domain root: its
        // negative guard resolves against the outer two-line domain first.
        assert_eq!(run(b"abc\ndefgh\n", ",x x/./ g-1 c/X/"), b"aXc\ndXfgh\n");
        for script in [",x g0", ",x g%0", ",x g1,0", ",x g999999999999999999999999"] {
            let mut text = Text::from_bytes(input);
            assert!(
                execute(&mut text, script, 0..input.len()).is_err(),
                "{script}"
            );
            assert_eq!(text.to_vec(), input);
        }
    }

    #[test]
    fn one_transcript_counts_across_inputs_and_retains_identity() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"a\nb\nc\nd\n");
        let dots = [0..2, 2..4, 4..6, 6..8];
        let result = session.execute_many(&mut text, "g2,3", &dots).unwrap();
        assert_eq!(result.selections, vec![2..4, 4..6]);
        assert_eq!(result.selection_inputs, vec![1, 2]);
        let result = session.execute_many(&mut text, "v-1 i/!/", &dots).unwrap();
        assert_eq!(text.to_vec(), b"!a\n!b\n!c\nd\n");
        assert_eq!(
            result
                .changes
                .iter()
                .map(|change| change.input)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(text.undo().is_some());
        assert_eq!(text.to_vec(), b"a\nb\nc\nd\n");
        assert!(session.execute_many(&mut text, "#0,#2c/X/", &dots).is_err());
        assert_eq!(text.to_vec(), b"a\nb\nc\nd\n");
    }

    #[test]
    fn unaddressed_extract_defaults_to_all_for_one_normal_cursor() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"a\nb\nc\nd\n");
        let result = session.execute_many(&mut text, "x g%2", &[0..0]).unwrap();
        assert_eq!(result.selections, vec![2..4, 6..8]);
        session.execute_many(&mut text, "a/>/", &[0..0]).unwrap();
        assert_eq!(text.to_vec(), b"a>\nb\nc\nd\n");
    }

    #[test]
    fn write_requests_preserve_raw_byte_addresses_and_quoted_paths() {
        let mut session = Session::new();
        let mut text = Text::from_bytes("♪\0last\n".as_bytes());
        let original = text.to_vec();
        let write = session
            .resolve_write(&text, "#1,#4w! 'a b' \"c d\"", &[0..0], false)
            .unwrap()
            .unwrap();
        assert_eq!(
            write,
            WriteRequest {
                force: true,
                quit: false,
                path: Some("'a b' \"c d\"".into()),
                ranges: Some(vec![1..4]),
                visual: false
            }
        );
        assert_eq!(text.to_vec(), original);
        assert!(!text.modified());
        assert_eq!(text.undo(), None);

        let text = Text::from_bytes(b"a\nbb\nccc\n");
        assert_eq!(
            session
                .resolve_write(&text, "2w out", &[0..0], false)
                .unwrap()
                .unwrap()
                .ranges,
            Some(vec![2..5])
        );
        let write = session
            .resolve_write(&text, "1,2wq out", &[0..0], false)
            .unwrap()
            .unwrap();
        assert!(write.quit);
        assert_eq!(write.ranges, Some(vec![0..5]));
        assert_eq!(
            session
                .resolve_write(&text, "write", &[0..0], false)
                .unwrap()
                .unwrap()
                .ranges,
            None
        );
    }

    #[test]
    fn visual_write_requests_stream_all_inputs_and_keep_restriction_metadata() {
        let mut session = Session::new();
        let text = Text::from_bytes(b"abcdef");
        let dots = [0..2, 4..6];
        for command in ["w out", "#2,#3w out"] {
            let write = session
                .resolve_write(&text, command, &dots, true)
                .unwrap()
                .unwrap();
            assert_eq!(write.ranges, Some(dots.to_vec()));
            assert!(write.visual);
        }
        let write = session
            .resolve_write(&text, "w", &[0..6], true)
            .unwrap()
            .unwrap();
        assert!(write.visual && write.path.is_none() && !write.force);
        assert_eq!(write.ranges, Some(vec![0..6]));
        let write = session
            .resolve_write(&text, ".w out", &dots, false)
            .unwrap()
            .unwrap();
        assert_eq!(
            write.ranges,
            Some(vec![0..2]),
            "C writes once using its first input"
        );
    }

    #[test]
    fn write_resolution_uses_stable_marks_and_preserves_parser_side_effects() {
        let mut session = Session::new();
        let text = Text::from_bytes(b"foo bar");
        session
            .set_marks(&text, 'a', vec![text.mark(1).unwrap()])
            .unwrap();
        session
            .set_marks(&text, 'b', vec![text.mark(4).unwrap()])
            .unwrap();
        assert_eq!(
            session
                .resolve_write(&text, "'a,'bw out", &[0..0], false)
                .unwrap()
                .unwrap()
                .ranges,
            Some(vec![1..4])
        );
        assert!(
            session
                .resolve_write(&text, "/foo/w 'unterminated", &[0..0], false)
                .is_err()
        );
        assert_eq!(session.previous_regex(), Some("foo"));
        assert!(
            session
                .resolve_write(&text, "#999w out", &[0..0], false)
                .is_err()
        );
        assert!(
            session
                .resolve_write(&text, "wincmd w", &[0..0], false)
                .unwrap()
                .is_none()
        );
        assert!(
            session
                .resolve_write(&text, "/foo/c/bar/", &[0..0], false)
                .unwrap()
                .is_none()
        );
        assert!(
            session
                .resolve_write(&text, "w out\nd", &[0..0], false)
                .is_err()
        );
        assert_eq!(text.to_vec(), b"foo bar");
    }

    #[test]
    fn session_regex_register_survives_later_parse_and_evaluation_failures() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"one two one");
        session.execute(&mut text, ",x/one/", 0..11).unwrap();
        assert_eq!(session.previous_regex(), Some("one"));
        session.execute(&mut text, ",x// c/X/", 0..11).unwrap();
        assert_eq!(text.to_vec(), b"X two X");
        assert!(session.execute(&mut text, ",x/two/ {", 0..7).is_err());
        assert_eq!(session.previous_regex(), Some("two"));
        assert!(session.execute(&mut text, ",x/[/ d", 0..7).is_err());
        assert_eq!(session.previous_regex(), Some("two"));
        assert!(session.execute(&mut text, ",x/X/c/Z/ #999d", 0..7).is_err());
        assert_eq!(session.previous_regex(), Some("X"));
        assert_eq!(text.to_vec(), b"X two X");
        session.set_previous_regex("two").unwrap();
        assert!(session.set_previous_regex("[").is_err());
        session.execute(&mut text, ",x// c/Y/", 0..7).unwrap();
        assert_eq!(text.to_vec(), b"X Y X");
    }

    #[test]
    #[cfg(unix)]
    fn session_shell_register_survives_a_later_parse_failure() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"original");
        assert!(
            session
                .execute(&mut text, "< printf first\n{", 0..8)
                .is_err()
        );
        assert_eq!(session.previous_shell(), Some("printf first"));
        assert_eq!(text.to_vec(), b"original");
        session.execute(&mut text, "<\n", 0..8).unwrap();
        assert_eq!(text.to_vec(), b"first");
        session
            .execute(&mut text, "| printf failed; exit 2\n", 0..5)
            .unwrap_err();
        assert_eq!(session.previous_shell(), Some("printf failed; exit 2"));
        assert_eq!(text.to_vec(), b"first");
    }

    #[test]
    fn stable_mark_addresses_are_per_document_and_per_input() {
        let mut session = Session::new();
        let mut first = Text::from_bytes(b"abcdef");
        let mut second = Text::from_bytes(b"0123");
        session
            .set_marks(&first, 'a', vec![first.mark(1).unwrap()])
            .unwrap();
        session
            .set_marks(&first, 'b', vec![first.mark(4).unwrap()])
            .unwrap();
        session
            .set_marks(&second, 'a', vec![second.mark(2).unwrap()])
            .unwrap();
        session.execute(&mut first, "'a,'bc/X/", 0..6).unwrap();
        assert_eq!(first.to_vec(), b"aXef");
        assert!(session.execute(&mut first, "'ai/!/", 0..4).is_err());
        first.undo().unwrap();
        session.execute(&mut first, "'ai/A/", 0..6).unwrap();
        session.execute(&mut second, "'ai/B/", 0..4).unwrap();
        assert_eq!(first.to_vec(), b"aAbcdef");
        assert_eq!(second.to_vec(), b"01B23");
        session.close_text_marks(&first);
        assert!(session.marks(&first, 'a').is_none());
        assert_eq!(session.marks(&second, 'a').unwrap().len(), 1);
        assert!(session.clear_marks(&second, 'a').is_some());
        assert!(session.execute(&mut second, "'ap", 0..5).is_err());

        let mut text = Text::from_bytes(b"abc def");
        session
            .set_marks(
                &text,
                'a',
                vec![text.mark(1).unwrap(), text.mark(5).unwrap()],
            )
            .unwrap();
        let result = session
            .execute_many(&mut text, "'ai/!/", &[0..3, 4..7])
            .unwrap();
        assert_eq!(text.to_vec(), b"a!bc d!ef");
        assert_eq!(
            result
                .changes
                .iter()
                .map(|change| change.input)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    #[cfg(unix)]
    fn session_directory_scopes_shell_and_file_reads_without_chdir() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("vis-sam-directory-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("read"), b"data").unwrap();
        let before = std::env::current_dir().unwrap();
        let mut session = Session::new();
        session.set_directory(directory.clone());
        let mut text = Text::new();
        let read = session.execute(&mut text, "0r read\n", 0..0);
        let shell = session.execute(&mut text, ",| cat read\n", 0..4);
        std::fs::remove_dir_all(&directory).unwrap();
        read.unwrap();
        shell.unwrap();
        assert_eq!(text.to_vec(), b"data");
        assert_eq!(session.directory(), Some(directory.as_path()));
        assert_eq!(std::env::current_dir().unwrap(), before);
    }

    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn assert_shell_cancellation(script: &str, input: &[u8]) {
        use std::time::{Duration, Instant};
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = std::env::temp_dir().join(format!(
            "vis-sam-cancel-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let directory = Cleanup(directory);
        let interrupt = Arc::new(AtomicBool::new(false));
        let mut session = Session::new();
        session.set_interrupt(Arc::clone(&interrupt));
        let groups = Arc::new(std::sync::Mutex::new(Vec::<Option<u32>>::new()));
        let observed_groups = Arc::clone(&groups);
        session.set_process_group_observer(Arc::new(move |group| {
            observed_groups.lock().unwrap().push(group);
            Ok(())
        }));
        session.set_directory(directory.0.clone());
        let mut text = Text::from_bytes(input);
        let len = text.len();
        let signal = Arc::clone(&interrupt);
        let ready = directory.0.join("ready");
        let trigger = std::thread::spawn(move || {
            let started = Instant::now();
            while !ready.exists() && started.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(5));
            }
            signal.store(true, Ordering::Relaxed);
            ready.exists()
        });
        let started = Instant::now();
        let error = session.execute(&mut text, script, 0..len).unwrap_err();
        assert!(
            trigger.join().unwrap(),
            "shell never reached its readiness checkpoint"
        );
        assert!(error.message.contains("cancelled"), "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cancellation took {:?}",
            started.elapsed()
        );
        assert_eq!(text.to_vec(), input);
        assert_eq!(text.undo(), None);
        {
            let groups = groups.lock().unwrap();
            assert_eq!(groups.len(), 2);
            assert!(groups[0].is_some());
            assert_eq!(groups[1], None);
        }
        // The host owns acknowledgement/reset; cancellation must not poison
        // the Session or its pipe tasks for the next command.
        interrupt.store(false, Ordering::Relaxed);
        session
            .execute(&mut text, ",| printf resumed\n", 0..len)
            .unwrap();
        assert_eq!(text.to_vec(), b"resumed");
    }

    #[test]
    #[cfg(all(
        unix,
        any(feature = "terminal", feature = "lua"),
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    fn shell_observer_continues_stopped_group_and_restores_only_after_reaping() {
        use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
        use std::time::Duration;
        let groups = Arc::new(std::sync::Mutex::new(Vec::<Option<u32>>::new()));
        let observed = Arc::clone(&groups);
        let mut session = Session::new();
        session.set_process_group_observer(Arc::new(move |group| {
            let mut events = observed.lock().unwrap();
            if let Some(group) = group {
                let pid = Pid::from_raw(i32::try_from(group).unwrap()).unwrap();
                // The runner must resume a group which tried terminal input
                // before the foreground handoff and was stopped with SIGTTIN.
                kill_process_group(pid, Signal::STOP)?;
                assert!(
                    waitid(
                        WaitId::Pid(pid),
                        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                    )
                    .is_ok()
                );
            } else {
                let pid = Pid::from_raw(i32::try_from(events[0].unwrap()).unwrap()).unwrap();
                assert_eq!(
                    waitid(
                        WaitId::Pid(pid),
                        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                    )
                    .unwrap_err(),
                    rustix::io::Errno::CHILD
                );
            }
            events.push(group);
            Ok(())
        }));
        let flag = Arc::new(AtomicBool::new(false));
        session.set_interrupt(Arc::clone(&flag));
        // A missing CONT must fail within a bound rather than hanging this test.
        let (done, timeout) = std::sync::mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if timeout.recv_timeout(Duration::from_secs(2)).is_err() {
                flag.store(true, Ordering::Relaxed);
            }
        });
        let mut text = Text::from_bytes(b"original");
        let result = session.execute(&mut text, ",| sleep 0.02; printf resumed\n", 0..8);
        let _ = done.send(());
        watchdog.join().unwrap();
        result.unwrap();
        assert_eq!(text.to_vec(), b"resumed");
        let groups = groups.lock().unwrap();
        assert_eq!(groups.len(), 2);
        assert!(groups[0].is_some());
        assert_eq!(groups[1], None);
    }

    #[test]
    #[cfg(all(
        unix,
        any(feature = "terminal", feature = "lua"),
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    fn shell_observer_errors_restore_after_cleanup_and_preserve_document() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, waitid};
        use std::time::{Duration, Instant};
        for failure in ["enter", "restore", "exit"] {
            let groups = Arc::new(std::sync::Mutex::new(Vec::<Option<u32>>::new()));
            let observed = Arc::clone(&groups);
            let mut session = Session::new();
            session.set_process_group_observer(Arc::new(move |group| {
                let mut events = observed.lock().unwrap();
                let first_restore = group.is_none() && events.len() == 1;
                if group.is_none() {
                    let pid = Pid::from_raw(i32::try_from(events[0].unwrap()).unwrap()).unwrap();
                    assert_eq!(
                        waitid(
                            WaitId::Pid(pid),
                            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                        )
                        .unwrap_err(),
                        rustix::io::Errno::CHILD
                    );
                }
                events.push(group);
                if (failure == "enter" && group.is_some())
                    || (failure == "restore" && first_restore)
                {
                    Err(io::Error::other(format!("{failure} observer failure")))
                } else {
                    Ok(())
                }
            }));
            let script = match failure {
                "enter" => "0i/queued/\n,| sleep 20\n",
                "restore" => "0i/queued/\n,| printf replacement\n",
                _ => "0i/queued/\n,| exit 23\n",
            };
            let mut text = Text::from_bytes(b"original");
            let started = Instant::now();
            let error = session.execute(&mut text, script, 0..8).unwrap_err();
            assert!(started.elapsed() < Duration::from_secs(3));
            if failure == "exit" {
                assert!(error.message.contains("23"), "{error}");
            } else {
                assert!(error.message.contains(failure), "{error}");
            }
            assert_eq!(text.to_vec(), b"original");
            assert_eq!(text.undo(), None);
            let events = groups.lock().unwrap();
            assert_eq!(events.len(), if failure == "restore" { 3 } else { 2 });
            assert!(events[0].is_some());
            assert!(events[1..].iter().all(Option::is_none));
        }
    }

    #[test]
    #[cfg(all(
        unix,
        any(feature = "terminal", feature = "lua"),
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    ))]
    fn shell_leader_remains_waitable_until_group_cleanup_preserves_status() {
        use rustix::process::{Pid, WaitId, WaitIdOptions, WaitOptions, waitid, waitpid};
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let directory = Cleanup(
            std::env::temp_dir().join(format!("vis-sam-group-owner-{}", std::process::id())),
        );
        std::fs::create_dir(&directory.0).unwrap();
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "(trap '' TERM; : > ready; sleep 0.3; : > survived) </dev/null >/dev/null 2>&1 & while [ ! -f ready ]; do :; done; exit 23",
            ])
            .current_dir(&directory.0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let observed = Cell::new(false);
        let started = Instant::now();
        let (status, cancelled) = wait_shell(&mut child, None, || {
            // This callback runs after exit observation, before final cleanup.
            // A try_wait here would already have released the leader's PID.
            assert!(
                waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
                )
                .unwrap()
                .is_some()
            );
            observed.set(true);
            true
        });
        assert!(observed.get());
        assert!(!cancelled);
        assert_eq!(status.unwrap().code(), Some(23));
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            waitpid(Some(pid), WaitOptions::NOHANG).unwrap_err(),
            rustix::io::Errno::CHILD
        );
        // Closed pipe descriptors must not let the ignored-TERM descendant
        // escape normal group cleanup when its shell leader already exited.
        std::thread::sleep(Duration::from_millis(400));
        assert!(!directory.0.join("survived").exists());
    }

    #[test]
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn cancel_shell_pipeline_transactionally_and_resume() {
        assert_shell_cancellation(
            "0i/queued/\n,| printf ready > ready; sleep 20 | cat\n",
            b"original",
        );
    }

    #[test]
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn cancel_shell_descendants_which_ignore_term() {
        assert_shell_cancellation(
            ",| trap '' TERM; (trap '' TERM; printf ready > ready; sleep 20) & wait\n",
            b"original",
        );
    }

    #[test]
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn cancel_shell_after_leader_exits_but_background_child_owns_pipes() {
        assert_shell_cancellation(
            ",| (sleep 0.05; printf ready > ready; sleep 20) & exit 0\n",
            b"original",
        );
    }

    #[test]
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn cancel_shell_with_blocked_large_pipe_input() {
        assert_shell_cancellation(
            ",| printf ready > ready; sleep 20\n",
            &vec![b'x'; 4 * 1024 * 1024],
        );
    }

    #[test]
    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    fn cancel_shell_while_draining_large_output_and_errors() {
        assert_shell_cancellation(
            ",| printf ready > ready; while :; do printf '0123456789012345678901234567890123456789'; printf error >&2; done\n",
            b"original",
        );
    }

    #[test]
    fn selected_original_characters_survive_boundary_insertions() {
        let mut text = Text::from_bytes(b"abcdef");
        let execution = execute(&mut text, "{ #1,#3p #1i/</ #3a/>/ }", 0..6).unwrap();
        assert_eq!(text.to_vec(), b"a<bc>def");
        assert_eq!(execution.selections, vec![2..4]);
        let execution = execute(&mut text, "{ #2,#4p #2,#3d }", 0..8).unwrap();
        assert!(execution.selections.is_empty());
    }

    #[test]
    fn selection_object_provenance_survives_mark_filtering() {
        let mut session = Session::new();
        let mut text = Text::from_bytes(b"a b c");
        for (script, detached) in [
            ("p", false),
            ("g/a/p", false),
            ("{p}", true),
            ("x/./", true),
        ] {
            let result = session.execute(&mut text, script, 0..1).unwrap();
            assert_eq!(result.selection_detached, vec![detached], "{script}");
            assert_eq!(result.selections, vec![0..1]);
            assert_eq!(result.selection_inputs, vec![0]);
        }
        let result = session
            .execute_many(&mut text, "p", &[0..1, 2..3, 4..5])
            .unwrap();
        assert_eq!(result.selection_detached, vec![false; 3]);
        assert_eq!(result.selection_inputs, vec![0, 1, 2]);
        let result = session
            .execute(&mut text, "{ #0,#1p #2,#3p #0,#1d }", 0..5)
            .unwrap();
        assert_eq!(result.selections, vec![1..2]);
        assert_eq!(result.selection_inputs, vec![0]);
        assert_eq!(result.selection_detached, vec![true]);
    }

    #[test]
    #[cfg(unix)]
    fn shell_filter_pipe_in_and_previous_command() {
        assert_eq!(
            run(b"Hello\nWorld\n", "1 | tr a-z A-Z\n2 |\n"),
            b"HELLO\nWORLD\n"
        );
        assert_eq!(run(b"one\ntwo\n", "1 < printf 'new\\n'\n"), b"new\ntwo\n");
        assert_eq!(run(b"original", "! printf ignored\n"), b"original");
        assert_eq!(run(b"original", "> cat\n"), b"original");
    }

    #[test]
    #[cfg(unix)]
    fn shell_failure_does_not_commit_prior_edits() {
        let mut text = Text::from_bytes(b"original");
        let error = execute(
            &mut text,
            "0i/first/\n,| printf partial; printf reason >&2; exit 3\n",
            0..8,
        )
        .unwrap_err();
        assert!(error.message.contains("reason"));
        assert_eq!(text.to_vec(), b"original");
        assert_eq!(text.undo(), None);
    }

    #[test]
    #[cfg(unix)]
    fn subprocess_pipes_are_drained_while_writing_input() {
        let input = vec![b'x'; 262_144];
        let output = run(&input, ",| head -c 262144 /dev/zero; cat\n");
        assert_eq!(&output[..262_144], vec![0; 262_144]);
        assert_eq!(&output[262_144..], input);
    }

    #[test]
    fn read_file_with_quoted_path() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "vis-sam-{}-{unique} with spaces",
            std::process::id()
        ));
        std::fs::write(&path, b"\0\xffread").unwrap();
        let mut text = Text::from_bytes(b"original");
        let result = execute(
            &mut text,
            &format!("0r {:?}\n", path.to_str().unwrap()),
            0..8,
        );
        std::fs::remove_file(path).unwrap();
        result.unwrap();
        assert_eq!(text.to_vec(), b"\0\xffreadoriginal");
    }

    #[test]
    fn resource_limits_and_unsupported_commands_are_errors() {
        let script = format!("{}i/x/{}", "{".repeat(70), "}".repeat(70));
        assert!(execute(&mut Text::new(), &script, 0..0).is_err());
        assert!(execute(&mut Text::new(), "i999999999/x/", 0..0).is_err());
        for script in ["s/a/b/", "w /tmp/file", "g0/x/d", "'ad"] {
            assert!(execute(&mut Text::new(), script, 0..0).is_err(), "{script}");
        }
    }
}

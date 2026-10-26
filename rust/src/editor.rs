//! Terminal-independent modal editor state.
//!
//! Byte positions and binary data follow the text core. Commands use the motion
//! and structural editing modules rather than invoking the C implementation.
//! The state machine owns ordered, oriented selections and edits them in one
//! validated transcript. The host owns windows, terminal rendering and file I/O.

pub mod actions;
pub use actions::{
    NATIVE_ACTIONS, NativeAction, NativeActionDefinition, NativeArgument, NativeCall,
    NativeHandler, NativeInfo, NativeMotion, NativeOperator, NativeSupport, NativeTextObject,
    NativeWindow,
};

use crate::{
    jumplist::JumpList,
    motion,
    regex::Regex,
    sam,
    selection::{Selection, SelectionId, SelectionMarks, SelectionSet},
    text::Text,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::ops::Range;

const MAX_COUNT: usize = 1_000_000;
const MAX_REPEAT_KEYS: usize = 1_000_000;
const MAX_INSERT_BYTES: usize = 64 * 1024 * 1024;
const PROMPT_REGISTER: char = '\0';

fn native_linewise_range(text: &Text, range: &Range<usize>) -> bool {
    range.start < range.end
        && (range.start == 0 || text.byte(range.start - 1) == Some(b'\n'))
        && text.byte(range.end - 1) == Some(b'\n')
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Key {
    Char(char),
    Escape,
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Ctrl(char),
    Tab,
    BackTab,
    Alt(char),
    AltCtrl(char),
    PageUp,
    PageDown,
    Insert,
    ShiftLeft,
    ShiftRight,
    ShiftPageUp,
    ShiftPageDown,
    Function(u8),
    /// Interned symbolic action; native terminal input never creates this key.
    Action(u64),
    Native(NativeAction),
    SelectionsSave,
    SelectionsRestore,
    PromptShow,
    PromptSearchForward,
    PromptSearchBackward,
    PromptSubmit,
    PromptCancel,
    PromptHistoryExpand,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Normal,
    Insert,
    Replace,
    Visual,
    VisualLine,
    OperatorPending,
    Command,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Insert => "INSERT",
            Self::Replace => "REPLACE",
            Self::Visual => "VISUAL",
            Self::VisualLine => "VISUAL LINE",
            Self::OperatorPending => "OPERATOR",
            Self::Command => "COMMAND",
        }
    }
}

pub type CallbackId = u64;
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum CallbackType {
    Mapping,
    Motion,
    Operator,
    TextObject,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallbackKind {
    Mapping { keys: String },
    Motion { pos: usize },
    Operator { range: Range<usize>, pos: usize },
    TextObject { pos: usize },
}
impl CallbackKind {
    fn callback_type(&self) -> CallbackType {
        match self {
            Self::Mapping { .. } => CallbackType::Mapping,
            Self::Motion { .. } => CallbackType::Motion,
            Self::Operator { .. } => CallbackType::Operator,
            Self::TextObject { .. } => CallbackType::TextObject,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallbackRequest {
    pub window: Option<u64>,
    pub token: u64,
    pub callback: CallbackId,
    pub kind: CallbackKind,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallbackResponse {
    Mapping(Option<isize>),
    Position(Option<usize>),
    Range(Option<Range<usize>>),
}
struct CallbackPending {
    request: CallbackRequest,
    selection: SelectionId,
    owner: crate::text::Mark,
    queued: Vec<(u64, usize)>,
    batch: Option<Box<CallbackBatch>>,
}
struct CallbackTask {
    selection: SelectionId,
    kind: CallbackKind,
    marks: Option<SelectionMarks>,
}
struct CallbackBatch {
    callback: CallbackId,
    kind: CallbackType,
    tasks: VecDeque<CallbackTask>,
    positions: BTreeMap<SelectionId, usize>,
    ranges: BTreeMap<SelectionId, Range<usize>>,
    primary: SelectionId,
    mode: Mode,
    operator: Option<Operator>,
    repeat: usize,
    remaining: usize,
    operator_count: usize,
    sources: BTreeMap<SelectionId, (Selection, SelectionMarks)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InputRequest {
    pub token: u64,
    pub bytes: Vec<u8>,
    pub replace: bool,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EditorAction {
    Continue,
    Callback(CallbackRequest),
    Input(InputRequest),
    PromptShow {
        title: Vec<u8>,
    },
    PromptSubmit,
    PromptCancel,
    PromptHistoryExpand,
    Quit {
        force: bool,
    },
    Save {
        quit: bool,
        force: bool,
        path: Option<String>,
        ranges: Option<Vec<Range<usize>>>,
        visual: bool,
    },
    Message(String),
    HostCommand(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct EditorError(pub String);

impl fmt::Display for EditorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for EditorError {}
impl From<crate::text::TextError> for EditorError {
    fn from(error: crate::text::TextError) -> Self {
        Self(error.to_string())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Register {
    pub bytes: Vec<u8>,
    pub linewise: bool,
}

#[derive(Clone, Copy, Debug)]
enum Operator {
    Delete,
    Change,
    Yank,
    ShiftLeft,
    ShiftRight,
    Callback(CallbackId),
}

/// Native selection case operation. C maps each wide character with
/// `towlower`/`towupper`; mappings that would change the byte length keep the
/// original character instead.
#[derive(Clone, Copy, Debug)]
pub enum CaseFold {
    Lower,
    Toggle,
    Upper,
}

impl CaseFold {
    fn apply(self, character: char) -> char {
        match self {
            Self::Lower => single_char(character.to_lowercase()).unwrap_or(character),
            Self::Upper => single_char(character.to_uppercase()).unwrap_or(character),
            Self::Toggle => match single_char(character.to_lowercase()) {
                Some(lower) if lower != character => lower,
                _ => single_char(character.to_uppercase()).unwrap_or(character),
            },
        }
    }
}

fn single_char(mut characters: impl Iterator<Item = char>) -> Option<char> {
    match (characters.next(), characters.next()) {
        (Some(character), None) => Some(character),
        _ => None,
    }
}

/// Native character-search target: the typed character plus the movement shape
/// remembered for `;`/`,` and the `totill` pseudo motions.
#[derive(Clone, Copy, Debug)]
struct Find {
    character: char,
    backwards: bool,
    till: bool,
    cross_line: bool,
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    None,
    Register,
    InsertRegister,
    G,
    ReplaceChar,
    Find {
        backwards: bool,
        till: bool,
        /// Native `to_left`/`to_right`/`till_left`/`till_right` search the whole
        /// document; the default `f`/`F`/`t`/`T` motions stay within the line.
        cross_line: bool,
    },
    TextObject {
        outer: bool,
    },
    Z,
    Window,
    /// Native insert mode `<C-x>` prefix for the slide actions.
    InsertSlide,
    /// Native `[`/`]` prefix for the bracket and parenthesis motions.
    Bracket {
        closing: bool,
    },
    Mark,
    MacroRecord,
    MacroPlay,
    InsertDigraph,
    VerbatimStart,
    Verbatim {
        base: u32,
        remaining: usize,
        value: u32,
        unicode: bool,
    },
}

#[derive(Clone, Copy, Debug)]
enum Prompt {
    Command,
    Search { backwards: bool },
}

#[derive(Clone, Copy)]
enum Placement {
    Start,
    End,
}

/// A sequential native operator is evaluated completely before any edit reaches
/// the document. Stable scratch marks model C's rebasing between selections;
/// replaying the validated edits preserves the real document's marks and undo.
struct LineOperatorPlan {
    text: Text,
    edits: Vec<(Range<usize>, Vec<u8>)>,
}
impl LineOperatorPlan {
    fn new(data: &[u8]) -> Self {
        Self {
            text: Text::from_bytes(data),
            edits: Vec::new(),
        }
    }
    fn replace(&mut self, range: Range<usize>, bytes: &[u8]) -> Result<(), EditorError> {
        if range.is_empty() && bytes.is_empty() {
            return Ok(());
        }
        if self.edits.len() >= MAX_REPEAT_KEYS {
            return Err(EditorError("native operator edit limit exceeded".into()));
        }
        self.text.replace(range.clone(), bytes)?;
        self.edits.push((range, bytes.to_vec()));
        Ok(())
    }
    fn shift(
        &mut self,
        range: Range<usize>,
        cursor: usize,
        right: bool,
        expand: bool,
        width: usize,
    ) -> Result<usize, EditorError> {
        let data = self.text.to_vec();
        let mut pos = motion::line_begin(&data, range.end);
        if pos == range.end {
            pos = motion::line_prev(&data, pos);
        }
        let multiple_lines = motion::line_prev(&data, pos) >= range.start;
        let indent = if expand {
            vec![b' '; width]
        } else {
            vec![b'\t']
        };
        let mut result = cursor;
        loop {
            let end = motion::line_end(&data, pos);
            pos = motion::line_begin(&data, end);
            let previous = pos;
            if right {
                if !multiple_lines || pos != end {
                    self.replace(pos..pos, &indent)?;
                    if pos <= cursor {
                        result += indent.len();
                    }
                }
            } else {
                let len = if data.get(pos) == Some(&b'\t') {
                    1
                } else {
                    data[pos..]
                        .iter()
                        .take_while(|byte| **byte == b' ')
                        .count()
                        .min(width)
                };
                self.replace(pos..pos + len, b"")?;
                if pos < cursor {
                    result = result.saturating_sub((cursor - pos).min(len));
                }
            }
            pos = motion::line_prev(&data, pos);
            if pos < range.start || pos == previous {
                break;
            }
        }
        Ok(result)
    }
    fn join(
        &mut self,
        range: Range<usize>,
        linewise: bool,
        space: bool,
    ) -> Result<usize, EditorError> {
        let data = self.text.to_vec();
        let mut pos = motion::line_begin(&data, range.end);
        if linewise && motion::range_is_linewise(&data, range.clone()) {
            let previous = motion::line_prev(&data, pos);
            if motion::line_prev(&data, previous) >= range.start {
                pos = previous;
            }
        }
        let mut mark = None;
        loop {
            let previous = pos;
            let end = motion::line_start(&data, pos);
            pos = motion::line_prev(&data, end);
            if pos < range.start || end <= pos {
                break;
            }
            self.replace(pos..end, b"")?;
            let prior = pos.checked_sub(1).and_then(|p| self.text.byte(p));
            if space
                && prior.is_some_and(|b| !b.is_ascii_whitespace() && b != 0x0b)
                && self.text.byte(pos).is_some_and(|b| b != b'\n')
            {
                self.replace(pos..pos, b" ")?;
            }
            if mark.is_none() {
                mark = self.text.mark(pos);
            }
            if pos == previous {
                break;
            }
        }
        Ok(mark
            .and_then(|mark| self.text.mark_get(mark))
            .unwrap_or(range.start))
    }
}

/// State whose marks refer to a particular Text allocation. Hosts swap this
/// alongside Text when focusing a different buffer; global registers stay put.
#[derive(Default)]
pub struct BufferState {
    saved: BTreeMap<char, Vec<SelectionMarks>>,
}

impl BufferState {
    pub fn marked_ranges(&self, text: &Text, name: char) -> Vec<Range<usize>> {
        let data = text.to_vec();
        self.saved
            .get(&name)
            .into_iter()
            .flatten()
            .filter_map(|m| m.restore(text))
            .filter_map(|s| s.range(&data).ok())
            .collect()
    }
    pub fn set_marked_ranges(
        &mut self,
        text: &Text,
        name: char,
        ranges: Vec<Range<usize>>,
    ) -> Result<(), EditorError> {
        if !name.is_ascii_lowercase() && !matches!(name, '\'' | '^') {
            return Err(EditorError("invalid mark name".into()));
        }
        let data = text.to_vec();
        let marks = ranges
            .into_iter()
            .map(|range| {
                Selection::from_range(&data, range, crate::selection::Direction::Forward)
                    .and_then(|s| s.save(text))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EditorError(e.to_string()))?;
        self.saved.insert(name, marks);
        Ok(())
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum KeyReplayKind {
    Dot,
    Macro,
}
struct KeyReplay {
    kind: KeyReplayKind,
    keys: VecDeque<Key>,
    outer: VecDeque<(u64, Key)>,
    previous_replaying: bool,
    pending_depth: usize,
}
enum InsertReplayStep {
    Open(bool),
    Key(Key),
    Finish,
}
#[derive(Clone)]
struct Mapping {
    mode: Mode,
    left: Vec<Key>,
    right: Vec<Key>,
    callback: Option<CallbackId>,
    window: Option<u64>,
}

/// Parent parser state suspended while a host edits its internal prompt file.
/// Text, view identities, callback requests and input queues remain host-owned.
#[derive(Clone)]
pub struct PromptActionState {
    mode: Mode,
    pending: Pending,
    operator: Option<Operator>,
    operator_count: usize,
    count: Option<usize>,
    selected_register: Option<char>,
    selected_mark: char,
    command_keys: Vec<Key>,
    recording_change: bool,
    repeatable: bool,
    preferred_column: Option<usize>,
    motion_targets: Vec<Selection>,
    insert_count: usize,
    insert_keys: Vec<Key>,
    open_line: Option<bool>,
    last_insert_newline: bool,
    prompt: Prompt,
    prompt_range: Option<Range<usize>>,
    prompt_ranges: Vec<Range<usize>>,
    prompt_count: usize,
    prompt_mode: Mode,
}

/// One buffer, active cursor and modal command parser. UI code may read these
/// public fields, but must keep cursor positions within the text byte length.
pub struct Editor {
    pub text: Text,
    pub cursor: usize,
    pub mode: Mode,
    pub command: String,
    pub tab_width: usize,
    pub expand_tab: bool,
    pub auto_indent: bool,
    anchor: usize,
    pending: Pending,
    operator: Option<Operator>,
    operator_count: usize,
    count: Option<usize>,
    selected_register: Option<char>,
    registers: BTreeMap<char, Register>,
    register_slots: BTreeMap<char, Vec<Register>>,
    prompt: Prompt,
    prompt_range: Option<Range<usize>>,
    prompt_ranges: Vec<Range<usize>>,
    prompt_count: usize,
    search: String,
    search_backwards: bool,
    last_find: Option<Find>,
    command_keys: Vec<Key>,
    last_change: Vec<Key>,
    recording_change: bool,
    replaying: bool,
    insert_count: usize,
    insert_keys: Vec<Key>,
    repeatable: bool,
    open_line: Option<bool>,
    preferred_column: Option<usize>,
    selection_set: SelectionSet,
    motion_targets: Vec<Selection>,
    /// Active window's native 32-slot jump cache; the workspace moves it in and
    /// out of the focused window's saved view state.
    jumplist: JumpList<Mode>,
    /// Body geometry the UI last measured for the focused window. Until a
    /// frame is drawn this keeps native vis's 80x23 no-terminal defaults.
    viewport: crate::view::ViewportMetrics,
    /// First display row the next frame should show, set by scroll actions.
    /// Consumed after one render so later cursor motions auto-follow again.
    scroll_request: Option<usize>,
    /// Native `escdelay` option.
    escape_delay: Option<std::time::Duration>,
    /// Native `ignorecase` option: the search ignores the case of the ASCII
    /// letters.
    ignorecase: bool,
    /// Native `loadmethod` option: how a file is read.
    loadmethod: Option<String>,
    /// Native `change256colors` option: use the alternative 256 color palette.
    change256colors: bool,
    /// Native `breakat` option: the characters that may end a line.
    breakat: std::sync::Arc<str>,
    /// Native `view->start_mark`: the first displayed row is tracked by a text
    /// mark, so edits above the view keep the visible content in place.
    view_anchor: Option<crate::text::Mark>,
    buffer_state: BufferState,
    selected_mark: char,
    session: sam::Session,
    match_word: bool,
    macro_record: Option<char>,
    macro_keys: Vec<Key>,
    last_macro: Option<char>,
    macro_depth: usize,
    macro_work: usize,
    mappings: Vec<Mapping>,
    language_map: BTreeMap<char, char>,
    language_map_disabled: bool,
    allocated_window_mapping_modes: BTreeSet<(u64, u8)>,
    mapping_keys: Vec<Key>,
    mapping_expanding: bool,
    mapping_depth: usize,
    mapping_work: usize,
    history: BTreeMap<char, Vec<String>>,
    history_index: Option<usize>,
    history_draft: String,
    prompt_mode: Mode,
    prompt_insert: bool,
    digraph_bytes: Vec<u8>,
    word_search: Option<Vec<u8>>,
    last_insert_newline: bool,
    anchored: BTreeMap<SelectionId, bool>,
    external_marks: BTreeMap<SelectionId, SelectionMarks>,
    callbacks: BTreeMap<CallbackId, CallbackType>,
    callback_pending: BTreeMap<u64, CallbackPending>,
    callback_token: u64,
    callback_wait: Option<(CallbackId, String)>,
    window_id: Option<u64>,
    actions: BTreeMap<String, CallbackId>,
    external_commands: BTreeSet<String>,
    external_options: BTreeSet<String>,
    queued_keys: VecDeque<(u64, Key)>,
    queue_token: u64,
    queue_frames: Vec<VecDeque<(u64, Key)>>,
    feeding_keys: bool,
    literal_queued: BTreeSet<u64>,
    recorded_queued: BTreeSet<u64>,
    suppress_command_record: bool,
    key_replays: Vec<KeyReplay>,
    driving_replay: bool,
    input_interception: bool,
    input_pending: BTreeMap<u64, InputRequest>,
    insert_replay: VecDeque<InsertReplayStep>,
    driving_insert_replay: bool,
    host_prompts: bool,
    internal_scope: bool,
}

impl Editor {
    pub fn new(text: Text) -> Self {
        let selection_set = SelectionSet::single(&[], 0).expect("zero is a valid cursor");
        Self {
            text,
            cursor: 0,
            mode: Mode::Normal,
            command: String::new(),
            tab_width: 8,
            expand_tab: false,
            auto_indent: false,
            anchor: 0,
            pending: Pending::None,
            operator: None,
            operator_count: 1,
            count: None,
            selected_register: None,
            registers: BTreeMap::new(),
            register_slots: BTreeMap::new(),
            prompt: Prompt::Command,
            prompt_range: None,
            prompt_ranges: Vec::new(),
            prompt_count: 1,
            search: String::new(),
            search_backwards: false,
            last_find: None,
            command_keys: Vec::new(),
            last_change: Vec::new(),
            recording_change: false,
            replaying: false,
            insert_count: 1,
            insert_keys: Vec::new(),
            repeatable: true,
            open_line: None,
            preferred_column: None,
            selection_set,
            motion_targets: Vec::new(),
            jumplist: JumpList::new(),
            viewport: crate::view::ViewportMetrics {
                width: 80,
                height: 23,
                top: 0,
                wrapcolumn: 0,
            },
            scroll_request: None,
            view_anchor: None,
            escape_delay: None,
            ignorecase: false,
            loadmethod: None,
            change256colors: false,
            breakat: std::sync::Arc::from(""),
            buffer_state: BufferState::default(),
            selected_mark: '\'',
            session: sam::Session::new(),
            match_word: false,
            macro_record: None,
            macro_keys: Vec::new(),
            last_macro: None,
            macro_depth: 0,
            macro_work: 0,
            mappings: Vec::new(),
            language_map: BTreeMap::new(),
            language_map_disabled: false,
            allocated_window_mapping_modes: BTreeSet::new(),
            mapping_keys: Vec::new(),
            mapping_expanding: false,
            mapping_depth: 0,
            mapping_work: 0,
            history: BTreeMap::new(),
            history_index: None,
            history_draft: String::new(),
            prompt_mode: Mode::Normal,
            prompt_insert: true,
            digraph_bytes: Vec::new(),
            word_search: None,
            last_insert_newline: false,
            anchored: BTreeMap::new(),
            external_marks: BTreeMap::new(),
            callbacks: BTreeMap::new(),
            callback_pending: BTreeMap::new(),
            callback_token: 0,
            callback_wait: None,
            window_id: None,
            actions: BTreeMap::new(),
            external_commands: BTreeSet::new(),
            external_options: BTreeSet::new(),
            queued_keys: VecDeque::new(),
            queue_token: 0,
            queue_frames: Vec::new(),
            feeding_keys: false,
            literal_queued: BTreeSet::new(),
            recorded_queued: BTreeSet::new(),
            suppress_command_record: false,
            key_replays: Vec::new(),
            driving_replay: false,
            input_interception: false,
            input_pending: BTreeMap::new(),
            insert_replay: VecDeque::new(),
            driving_insert_replay: false,
            host_prompts: false,
            internal_scope: false,
        }
    }

    pub fn set_host_prompts(&mut self, enabled: bool) {
        self.host_prompts = enabled;
    }
    pub fn set_internal_scope(&mut self, internal: bool) {
        self.internal_scope = internal;
    }
    pub fn set_mode_raw(&mut self, mode: Mode) {
        self.mode = mode;
    }
    pub fn install_view_state_raw(
        &mut self,
        selections: Vec<(SelectionId, Selection, bool)>,
        primary: SelectionId,
    ) -> Result<(), EditorError> {
        let anchored = selections
            .iter()
            .map(|(id, _, flag)| (*id, *flag))
            .collect();
        let set = SelectionSet::from_identified(
            &self.text.to_vec(),
            selections.into_iter().map(|(id, s, _)| (id, s)).collect(),
            primary,
        )
        .map_err(|e| EditorError(e.to_string()))?;
        self.selection_set = set;
        self.anchored = anchored;
        self.sync_set_to_primary();
        Ok(())
    }
    pub fn take_prompt_action_state(&mut self) -> PromptActionState {
        let state = PromptActionState {
            mode: self.mode,
            pending: self.pending,
            operator: self.operator,
            operator_count: self.operator_count,
            count: self.count,
            selected_register: self.selected_register,
            selected_mark: self.selected_mark,
            command_keys: std::mem::take(&mut self.command_keys),
            recording_change: self.recording_change,
            repeatable: self.repeatable,
            preferred_column: self.preferred_column,
            motion_targets: std::mem::take(&mut self.motion_targets),
            insert_count: self.insert_count,
            insert_keys: std::mem::take(&mut self.insert_keys),
            open_line: self.open_line,
            last_insert_newline: self.last_insert_newline,
            prompt: self.prompt,
            prompt_range: self.prompt_range.take(),
            prompt_ranges: std::mem::take(&mut self.prompt_ranges),
            prompt_count: self.prompt_count,
            prompt_mode: self.prompt_mode,
        };
        self.pending = Pending::None;
        self.operator = None;
        self.operator_count = 1;
        self.count = None;
        self.selected_register = None;
        self.selected_mark = '\'';
        self.recording_change = false;
        self.repeatable = true;
        self.preferred_column = None;
        self.insert_count = 1;
        self.open_line = None;
        self.last_insert_newline = false;
        state
    }
    pub fn restore_prompt_action_state(&mut self, state: PromptActionState) {
        self.mode = state.mode;
        self.pending = state.pending;
        self.operator = state.operator;
        self.operator_count = state.operator_count;
        self.count = state.count;
        self.selected_register = state.selected_register;
        self.selected_mark = state.selected_mark;
        self.command_keys = state.command_keys;
        self.recording_change = state.recording_change;
        self.repeatable = state.repeatable;
        self.preferred_column = state.preferred_column;
        self.motion_targets = state.motion_targets;
        self.insert_count = state.insert_count;
        self.insert_keys = state.insert_keys;
        self.open_line = state.open_line;
        self.last_insert_newline = state.last_insert_newline;
        self.prompt = state.prompt;
        self.prompt_range = state.prompt_range;
        self.prompt_ranges = state.prompt_ranges;
        self.prompt_count = state.prompt_count;
        self.prompt_mode = state.prompt_mode;
    }
    pub fn prompt_history_up(&mut self) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let positions = self
            .selections()
            .iter()
            .map(|s| motion::line_up(&data, s.cursor))
            .collect();
        self.set_cursors(positions, self.primary_index())
    }
    fn operation_register_name(&self) -> char {
        self.selected_register.unwrap_or(if self.internal_scope {
            PROMPT_REGISTER
        } else {
            '"'
        })
    }
    pub fn submit_search(
        &mut self,
        pattern: &str,
        backwards: bool,
    ) -> Result<EditorAction, EditorError> {
        if pattern.is_empty() {
            return Ok(EditorAction::Continue);
        }
        if let Err(error) = self.search_regex(pattern) {
            // C vis_cancel resets the action, without view or mode hooks.
            self.pending = Pending::None;
            self.operator = None;
            self.operator_count = 1;
            self.count = None;
            self.selected_register = None;
            self.selected_mark = '\'';
            self.motion_targets.clear();
            self.command_keys.clear();
            self.recording_change = false;
            return Err(EditorError(error.to_string()));
        }
        self.session
            .set_previous_regex(pattern)
            .map_err(|e| EditorError(e.to_string()))?;
        self.search = pattern.to_owned();
        self.search_backwards = backwards;
        self.word_search = None;
        self.registers.insert(
            '/',
            Register {
                bytes: pattern.as_bytes().to_vec(),
                linewise: false,
            },
        );
        if !self.replaying {
            self.command_keys.extend(pattern.chars().map(Key::Char));
            self.command_keys.push(Key::Enter);
        }
        let count = self.combined_count()?;
        let source = self.cursor;
        let mut targets = Vec::new();
        for s in self.selections().to_vec() {
            self.cursor = s.cursor;
            for _ in 0..count {
                if self.search_motion(backwards).is_err() {
                    self.cursor = s.cursor;
                    break;
                }
            }
            targets.push(Selection {
                anchor: if self.is_visual() {
                    s.anchor
                } else {
                    self.cursor
                },
                cursor: self.cursor,
            });
        }
        self.cursor = source;
        self.motion_targets = targets;
        let action = self.finish_motion(source, false, false)?;
        self.sync_primary_to_set()?;
        if self.mode == Mode::Normal && self.operator.is_none() {
            if !self.internal_scope && !self.replaying && self.recording_change && self.repeatable {
                self.last_change = std::mem::take(&mut self.command_keys);
            }
            self.command_keys.clear();
            self.recording_change = false;
            self.selected_register = None;
        }
        Ok(action)
    }
    fn enqueue_keys(&mut self, keys: Vec<Key>) -> Result<(), EditorError> {
        if self.queued_keys.len().saturating_add(keys.len()) > MAX_REPEAT_KEYS {
            return Err(EditorError("input queue limit exceeded".into()));
        }
        for key in keys {
            self.queue_token = self
                .queue_token
                .checked_add(1)
                .ok_or_else(|| EditorError("input queue token limit exceeded".into()))?;
            self.queued_keys.push_back((self.queue_token, key));
        }
        Ok(())
    }
    /// Feed one owned symbolic input batch, stopping at host/callback requests.
    pub fn feed_keys(&mut self, keys: Vec<Key>) -> Result<EditorAction, EditorError> {
        if !self.callback_pending.is_empty() || !self.input_pending.is_empty() {
            self.queue_frames
                .push(std::mem::take(&mut self.queued_keys));
        }
        let waiting = self.callback_wait.is_some();
        if waiting && !self.replaying {
            if self.command_keys.len().saturating_add(keys.len()) > MAX_REPEAT_KEYS {
                return Err(EditorError("command key limit exceeded".into()));
            }
            self.command_keys.extend(keys.iter().copied());
        }
        let first_token = self.queue_token;
        self.enqueue_keys(keys)?;
        if waiting {
            for (id, _) in &self.queued_keys {
                if *id > first_token {
                    self.recorded_queued.insert(*id);
                }
            }
        }
        if let Some((id, _)) = self.callback_wait.take() {
            return self.request_callback(
                CallbackKind::Mapping {
                    keys: self.input_queue(),
                },
                id,
            );
        }
        self.next_queued()
    }
    pub fn next_queued(&mut self) -> Result<EditorAction, EditorError> {
        let frame_depth = self.queue_frames.len();
        if !self.insert_replay.is_empty()
            && !self.driving_insert_replay
            && self.input_pending.is_empty()
        {
            return self.drive_insert_replay();
        }
        if self.callback_wait.is_some() {
            if !self.key_replays.is_empty() && !self.driving_replay {
                return self.drive_replay();
            }
            return Ok(EditorAction::Continue);
        }
        while let Some((id, key)) = self.queued_keys.pop_front() {
            let was_feeding = self.feeding_keys;
            let was_expanding = self.mapping_expanding;
            let was_recorded = self.suppress_command_record;
            self.suppress_command_record |= self.recorded_queued.remove(&id);
            let literal = self.literal_queued.remove(&id);
            self.suppress_command_record |= literal;
            self.mapping_expanding |= literal;
            self.feeding_keys = true;
            let result = self.handle_key(key);
            self.feeding_keys = was_feeding;
            self.mapping_expanding = was_expanding;
            self.suppress_command_record = was_recorded;
            let action = result?;
            if action != EditorAction::Continue {
                return Ok(action);
            }
            if self.queue_frames.len() < frame_depth {
                return Ok(EditorAction::Continue);
            }
        }
        if !self.key_replays.is_empty() && !self.driving_replay {
            return self.drive_replay();
        }
        if let Some(outer) = self.queue_frames.pop() {
            self.queued_keys = outer;
        }
        Ok(EditorAction::Continue)
    }
    pub fn set_input_interception(&mut self, enabled: bool) {
        self.input_interception = enabled;
    }
    pub fn resume_input(
        &mut self,
        token: u64,
        consumed: bool,
    ) -> Result<EditorAction, EditorError> {
        let request = self
            .input_pending
            .remove(&token)
            .ok_or_else(|| EditorError("unknown or already completed input token".into()))?;
        if !consumed {
            self.insert_raw(&request.bytes, request.replace)?;
        }
        Ok(EditorAction::Continue)
    }
    pub fn cancel_input(&mut self, token: u64) -> bool {
        self.input_pending.remove(&token).is_some()
    }
    fn request_input(&mut self, c: char) -> Result<EditorAction, EditorError> {
        self.callback_token = self
            .callback_token
            .checked_add(1)
            .ok_or_else(|| EditorError("input token limit exceeded".into()))?;
        let mut encoded = [0; 4];
        let request = InputRequest {
            token: self.callback_token,
            bytes: c.encode_utf8(&mut encoded).as_bytes().to_vec(),
            replace: self.mode == Mode::Replace,
        };
        self.input_pending.insert(request.token, request.clone());
        Ok(EditorAction::Input(request))
    }
    pub fn set_window_id(&mut self, id: Option<u64>) {
        self.window_id = id;
    }
    pub fn register_external_option(&mut self, name: &str) {
        self.external_options.insert(name.to_owned());
    }
    pub fn unregister_external_option(&mut self, name: &str) -> bool {
        self.external_options.remove(name)
    }
    pub fn unregister_external_command(&mut self, name: &str) -> bool {
        self.external_commands.remove(name)
    }
    pub fn register_external_command(&mut self, name: &str) {
        self.external_commands.insert(name.to_owned());
    }
    pub fn register_callback(
        &mut self,
        kind: CallbackType,
        id: CallbackId,
    ) -> Result<(), EditorError> {
        if self.callbacks.contains_key(&id) {
            return Err(EditorError("callback already registered".into()));
        }
        self.callbacks.insert(id, kind);
        Ok(())
    }
    pub fn action_exists(&self, name: &str) -> bool {
        self.actions.contains_key(name) || NativeAction::from_name(name).is_some()
    }
    pub fn register_action(&mut self, name: &str, id: CallbackId) -> Result<(), EditorError> {
        if self.callbacks.get(&id) != Some(&CallbackType::Mapping) {
            return Err(EditorError("action needs a mapping callback".into()));
        }
        if name.is_empty() || name.contains(['<', '>']) {
            return Err(EditorError("invalid action name".into()));
        }
        if self.action_exists(name) {
            return Err(EditorError("action name already exists".into()));
        }
        self.actions.insert(name.into(), id);
        Ok(())
    }
    pub fn parse_keys(&self, mut input: &str) -> Result<Vec<Key>, EditorError> {
        let mut result = Vec::new();
        while !input.is_empty() {
            if let Some(end) = input
                .strip_prefix('<')
                .and_then(|s| s.find('>').map(|n| n + 1))
            {
                let token = &input[..=end];
                if let Some(id) = self.actions.get(&input[1..end]) {
                    result.push(Key::Action(*id));
                } else {
                    result.extend(Self::notation_keys(token)?);
                }
                input = &input[end + 1..];
            } else {
                let c = input.chars().next().expect("nonempty input");
                result.push(Key::Char(c));
                input = &input[c.len_utf8()..];
            }
        }
        Ok(result)
    }
    fn symbolic_key(&self, key: Key) -> String {
        if let Key::Action(id) = key {
            if let Some((name, _)) = self.actions.iter().find(|(_, candidate)| **candidate == id) {
                return format!("<{name}>");
            }
        }
        Self::key_notation(key)
    }
    pub fn map_callback(
        &mut self,
        mode: Mode,
        keys: &str,
        id: CallbackId,
        window: Option<u64>,
    ) -> Result<(), EditorError> {
        if self.callbacks.get(&id) != Some(&CallbackType::Mapping) {
            return Err(EditorError("mapping callback is not registered".into()));
        }
        let left = self.parse_keys(keys)?;
        self.install_mapping(Mapping {
            mode,
            left,
            right: Vec::new(),
            callback: Some(id),
            window,
        })
    }
    pub fn map_alias(
        &mut self,
        mode: Mode,
        keys: &str,
        right: &str,
        window: Option<u64>,
    ) -> Result<(), EditorError> {
        let left = self.parse_keys(keys)?;
        let right = self.parse_keys(right)?;
        self.install_mapping(Mapping {
            mode,
            left,
            right,
            callback: None,
            window,
        })
    }
    fn install_mapping(&mut self, mapping: Mapping) -> Result<(), EditorError> {
        if mapping.left.is_empty() {
            return Err(EditorError("empty mapping prefix".into()));
        }
        if let Some(window) = mapping.window {
            self.allocated_window_mapping_modes
                .insert((window, mapping.mode as u8));
        }
        self.mappings.retain(|old| {
            old.mode != mapping.mode || old.left != mapping.left || old.window != mapping.window
        });
        self.mappings.push(mapping);
        Ok(())
    }
    /// C split copies only allocated local binding tables. An allocated empty
    /// source table replaces a target table created by WIN_OPEN callbacks.
    pub fn copy_window_mappings(&mut self, source: u64, target: u64) {
        if source == target {
            return;
        }
        let modes: Vec<_> = self
            .allocated_window_mapping_modes
            .iter()
            .filter_map(|(window, mode)| (*window == source).then_some(*mode))
            .collect();
        for mode in modes {
            let mappings: Vec<_> = self
                .mappings
                .iter()
                .filter(|mapping| mapping.window == Some(source) && mapping.mode as u8 == mode)
                .cloned()
                .map(|mut mapping| {
                    mapping.window = Some(target);
                    mapping
                })
                .collect();
            self.mappings
                .retain(|mapping| mapping.window != Some(target) || mapping.mode as u8 != mode);
            self.mappings.extend(mappings);
            self.allocated_window_mapping_modes.insert((target, mode));
        }
    }
    pub fn forget_window_mappings(&mut self, window: u64) {
        self.mappings
            .retain(|mapping| mapping.window != Some(window));
        self.allocated_window_mapping_modes
            .retain(|(owner, _)| *owner != window);
    }
    pub fn mapping_aliases(
        &self,
        mode: Mode,
        window: Option<u64>,
    ) -> Vec<(String, Option<String>, Option<CallbackId>)> {
        self.mappings
            .iter()
            .filter(|m| m.mode == mode && m.window == window)
            .map(|m| {
                (
                    m.left.iter().map(|key| Self::key_notation(*key)).collect(),
                    m.callback
                        .is_none()
                        .then(|| m.right.iter().map(|key| Self::key_notation(*key)).collect()),
                    m.callback,
                )
            })
            .collect()
    }
    pub fn unmap(
        &mut self,
        mode: Mode,
        keys: &str,
        window: Option<u64>,
    ) -> Result<bool, EditorError> {
        let left = self.parse_keys(keys)?;
        let old = self.mappings.len();
        self.mappings
            .retain(|m| m.mode != mode || m.left != left || m.window != window);
        Ok(old != self.mappings.len())
    }
    pub fn invoke_callback(
        &mut self,
        kind: CallbackKind,
        id: CallbackId,
    ) -> Result<EditorAction, EditorError> {
        if let CallbackKind::Operator { .. } = kind {
            if !self.is_visual() {
                if self.callbacks.get(&id) != Some(&CallbackType::Operator) {
                    return Err(EditorError("unknown operator callback".into()));
                }
                self.operator = Some(Operator::Callback(id));
                self.operator_count = 1;
                self.mode = Mode::OperatorPending;
                return Ok(EditorAction::Continue);
            }
        }
        if matches!(kind, CallbackKind::Mapping { .. }) {
            return self.request_callback(kind, id);
        }
        let callback_type = kind.callback_type();
        let repeat = if callback_type == CallbackType::Operator {
            1
        } else {
            self.combined_count()?
        };
        let ranges = if callback_type == CallbackType::Operator {
            Some(self.selection_ranges())
        } else {
            None
        };
        self.start_callback_batch(id, callback_type, repeat, ranges)
    }
    fn start_callback_batch(
        &mut self,
        id: CallbackId,
        kind: CallbackType,
        repeat: usize,
        ranges: Option<Vec<Range<usize>>>,
    ) -> Result<EditorAction, EditorError> {
        if repeat
            .checked_mul(self.selections().len())
            .is_none_or(|work| work > MAX_REPEAT_KEYS)
        {
            return Err(EditorError("callback work limit exceeded".into()));
        }
        if self.callbacks.get(&id) != Some(&kind) {
            return Err(EditorError(
                "unknown callback or callback type mismatch".into(),
            ));
        }
        let mut tasks = VecDeque::new();
        let data = self.text.to_vec();
        for (index, (selection, s)) in self.view_identified().into_iter().enumerate() {
            let (task, marks) = match kind {
                CallbackType::Motion => (CallbackKind::Motion { pos: s.cursor }, None),
                CallbackType::TextObject => (CallbackKind::TextObject { pos: s.cursor }, None),
                CallbackType::Operator => {
                    let range = ranges
                        .as_ref()
                        .and_then(|ranges| ranges.get(index))
                        .cloned()
                        .unwrap_or(s.cursor..s.cursor);
                    let marks = Selection::from_range(
                        &data,
                        range.clone(),
                        crate::selection::Direction::Forward,
                    )
                    .and_then(|s| s.save(&self.text))
                    .ok();
                    (
                        CallbackKind::Operator {
                            range,
                            pos: s.cursor,
                        },
                        marks,
                    )
                }
                CallbackType::Mapping => {
                    return Err(EditorError(
                        "mapping callbacks cannot be selection batches".into(),
                    ));
                }
            };
            tasks.push_back(CallbackTask {
                selection,
                kind: task,
                marks,
            });
        }
        let sources = self
            .view_identified()
            .into_iter()
            .map(|(id, s)| s.save(&self.text).map(|marks| (id, (s, marks))))
            .collect::<Result<_, _>>()
            .map_err(|e| EditorError(e.to_string()))?;
        let batch = CallbackBatch {
            sources,
            callback: id,
            kind,
            tasks,
            positions: BTreeMap::new(),
            ranges: if kind == CallbackType::TextObject && self.is_visual() {
                self.view_identified()
                    .into_iter()
                    .filter_map(|(id, s)| s.range(&data).ok().map(|r| (id, r)))
                    .collect()
            } else {
                BTreeMap::new()
            },
            primary: self.selection_set.primary_id(),
            mode: self.mode,
            operator: self.operator.take(),
            operator_count: self.operator_count,
            repeat: repeat.max(1),
            remaining: repeat.max(1),
        };
        self.issue_callback_batch(batch)
    }
    fn issue_callback_batch(
        &mut self,
        mut batch: CallbackBatch,
    ) -> Result<EditorAction, EditorError> {
        while let Some(mut task) = batch.tasks.pop_front() {
            if self.selection_by_id(task.selection).is_none() {
                continue;
            }
            if let Some(s) = self.selection_by_id(task.selection) {
                match &mut task.kind {
                    CallbackKind::Motion { pos } | CallbackKind::TextObject { pos }
                        if batch.remaining == batch.repeat =>
                    {
                        *pos = s.cursor
                    }
                    _ => {}
                }
            }
            if let Some(marks) = task.marks {
                if let Some(selection) = marks.restore(&self.text) {
                    if let CallbackKind::Operator { range, pos } = &mut task.kind {
                        *range = selection
                            .range(&self.text.to_vec())
                            .map_err(|e| EditorError(e.to_string()))?;
                        *pos = self
                            .selection_by_id(task.selection)
                            .map_or(selection.cursor, |s| s.cursor);
                    }
                } else if let CallbackKind::Operator { range, pos } = &mut task.kind {
                    let Some(cursor) = self.selection_position(task.selection) else {
                        self.remove_selection(task.selection)?;
                        continue;
                    };
                    *range = cursor..cursor;
                    *pos = cursor;
                }
            }
            let action = self.request_callback(task.kind, batch.callback)?;
            let pending = self
                .callback_pending
                .get_mut(&self.callback_token)
                .expect("just inserted callback request");
            pending.selection = task.selection;
            pending.batch = Some(Box::new(batch));
            return Ok(action);
        }
        self.finish_callback_batch(batch)
    }
    fn finish_callback_batch(&mut self, batch: CallbackBatch) -> Result<EditorAction, EditorError> {
        if self.selection_by_id(batch.primary).is_some() {
            self.set_primary_selection(batch.primary)?;
        }
        self.operator = batch.operator;
        self.operator_count = batch.operator_count;
        self.mode = batch.mode;
        match batch.kind {
            CallbackType::Motion => {
                if self.operator.is_some() {
                    let ranges = self
                        .view_identified()
                        .into_iter()
                        .map(|(id, s)| {
                            let source = batch
                                .sources
                                .get(&id)
                                .map(|(fallback, marks)| {
                                    marks
                                        .restore(&self.text)
                                        .unwrap_or(*fallback)
                                        .cursor
                                        .min(self.text.len())
                                })
                                .unwrap_or(s.cursor);
                            source.min(s.cursor)..source.max(s.cursor)
                        })
                        .collect::<Vec<_>>();
                    if ranges.len() > 1 {
                        return self.apply_many_operators(ranges, false);
                    }
                    return self.apply_operator(ranges[0].clone(), false);
                }
                self.selection_set
                    .normalize(&self.text.to_vec())
                    .map_err(|e| EditorError(e.to_string()))?;
                self.sync_set_to_primary();
                self.materialize_visual_lines()?;
                Ok(EditorAction::Continue)
            }
            CallbackType::TextObject => {
                if batch.ranges.is_empty() {
                    return Ok(EditorAction::Continue);
                }
                let ranges = self
                    .view_identified()
                    .into_iter()
                    .map(|(id, s)| {
                        if batch.ranges.contains_key(&id) {
                            s.range(&self.text.to_vec()).unwrap_or(s.cursor..s.cursor)
                        } else {
                            s.cursor..s.cursor
                        }
                    })
                    .collect::<Vec<_>>();
                if self.operator.is_some() {
                    if ranges.len() > 1 {
                        self.apply_many_operators(ranges, false)
                    } else {
                        self.apply_operator(ranges[0].clone(), false)
                    }
                } else {
                    if !self.is_visual() {
                        self.mode = Mode::Visual;
                    }
                    for id in batch.ranges.keys() {
                        if self.selection_by_id(*id).is_some() {
                            self.anchored.insert(*id, true);
                        }
                    }
                    self.selection_set
                        .normalize(&self.text.to_vec())
                        .map_err(|e| EditorError(e.to_string()))?;
                    self.sync_set_to_primary();
                    self.materialize_visual_lines()?;
                    Ok(EditorAction::Continue)
                }
            }
            CallbackType::Operator => {
                self.operator = None;
                self.mode = Mode::Normal;
                self.count = None;
                for (id, _) in batch.positions {
                    if let Some(s) = self.selection_by_id(id) {
                        let pos = s.cursor;
                        self.update_selection(
                            id,
                            Selection {
                                anchor: pos,
                                cursor: pos,
                            },
                        )?;
                        self.anchored.insert(id, false);
                    }
                }
                self.selection_set
                    .normalize(&self.text.to_vec())
                    .map_err(|e| EditorError(e.to_string()))?;
                self.sync_set_to_primary();
                self.text.snapshot();
                Ok(EditorAction::Continue)
            }
            CallbackType::Mapping => Err(EditorError("invalid selection callback batch".into())),
        }
    }
    fn resume_callback_batch(
        &mut self,
        mut batch: CallbackBatch,
        selection: SelectionId,
        kind: CallbackKind,
        response: CallbackResponse,
    ) -> Result<EditorAction, EditorError> {
        let next = match (kind, response) {
            (CallbackKind::Motion { pos: previous }, CallbackResponse::Position(Some(pos))) => {
                if pos > self.text.len() {
                    return Err(EditorError("callback position exceeds text length".into()));
                }
                batch.positions.insert(selection, pos);
                let current = self
                    .selection_by_id(selection)
                    .ok_or_else(|| EditorError("callback selection removed".into()))?;
                self.update_selection(
                    selection,
                    Selection {
                        anchor: if matches!(batch.mode, Mode::Visual | Mode::VisualLine) {
                            current.anchor
                        } else {
                            pos
                        },
                        cursor: pos,
                    },
                )?;
                if pos == previous {
                    None
                } else {
                    Some(CallbackKind::Motion { pos })
                }
            }
            (CallbackKind::Operator { .. }, CallbackResponse::Position(pos)) => {
                if let Some(pos) = pos {
                    if pos > self.text.len() {
                        return Err(EditorError("callback position exceeds text length".into()));
                    }
                    batch.positions.insert(selection, pos);
                    self.update_selection(
                        selection,
                        Selection {
                            anchor: pos,
                            cursor: pos,
                        },
                    )?;
                } else {
                    self.remove_selection(selection)?;
                }
                None
            }
            (CallbackKind::TextObject { .. }, CallbackResponse::Range(Some(range))) => {
                if range.start > range.end || range.end > self.text.len() {
                    return Err(EditorError("invalid callback text object range".into()));
                }
                if let Some(previous) = batch.ranges.get_mut(&selection) {
                    if let Some(s) = self.selection_by_id(selection) {
                        if let Ok(range) = s.range(&self.text.to_vec()) {
                            *previous = range;
                        }
                    }
                }
                batch
                    .ranges
                    .entry(selection)
                    .and_modify(|old| {
                        old.start = old.start.min(range.start);
                        old.end = old.end.max(range.end);
                    })
                    .or_insert(range.clone());
                let combined = batch
                    .ranges
                    .get(&selection)
                    .cloned()
                    .expect("just inserted range");
                let s = Selection::from_range(
                    &self.text.to_vec(),
                    combined,
                    self.selection_by_id(selection)
                        .map_or(crate::selection::Direction::Forward, |s| s.direction()),
                )
                .map_err(|e| EditorError(e.to_string()))?;
                self.update_selection(selection, s)?;
                Some(CallbackKind::TextObject {
                    pos: batch.ranges[&selection].end,
                })
            }
            (CallbackKind::Motion { .. }, CallbackResponse::Position(None)) => {
                if let Some((fallback, marks)) = batch.sources.get(&selection) {
                    let cursor = marks
                        .cursor_position(&self.text)
                        .unwrap_or(fallback.cursor.min(self.text.len()));
                    self.update_selection(
                        selection,
                        Selection {
                            anchor: if matches!(batch.mode, Mode::Visual | Mode::VisualLine) {
                                self.selection_by_id(selection).map_or(cursor, |s| s.anchor)
                            } else {
                                cursor
                            },
                            cursor,
                        },
                    )?;
                    batch.positions.insert(selection, cursor);
                }
                None
            }
            (CallbackKind::TextObject { .. }, CallbackResponse::Range(None)) => None,
            _ => return Err(EditorError("callback response type mismatch".into())),
        };
        if let Some(kind) = next.filter(|_| batch.remaining > 1) {
            batch.remaining -= 1;
            batch.tasks.push_front(CallbackTask {
                selection,
                kind,
                marks: None,
            });
        } else {
            batch.remaining = batch.repeat;
        }
        self.issue_callback_batch(batch)
    }

    fn request_callback(
        &mut self,
        kind: CallbackKind,
        id: CallbackId,
    ) -> Result<EditorAction, EditorError> {
        if self.callbacks.get(&id) != Some(&kind.callback_type()) {
            return Err(EditorError(
                "callback type mismatch or unknown callback".into(),
            ));
        }
        self.callback_token = self
            .callback_token
            .checked_add(1)
            .ok_or_else(|| EditorError("callback token limit exceeded".into()))?;
        let request = CallbackRequest {
            window: self.window_id,
            token: self.callback_token,
            callback: id,
            kind,
        };
        let queued = self
            .queued_keys
            .iter()
            .map(|(id, key)| (*id, self.symbolic_key(*key).len()))
            .collect();
        let pending = CallbackPending {
            request: request.clone(),
            queued,
            batch: None,
            selection: self.selection_set.primary_id(),
            owner: self
                .text
                .mark(self.text.len())
                .ok_or_else(|| EditorError("unable to mark callback document".into()))?,
        };
        self.callback_pending.insert(request.token, pending);
        Ok(EditorAction::Callback(request))
    }
    pub fn cancel_callback(&mut self, token: u64) -> bool {
        self.abort_callback(token)
    }
    pub fn abort_callback(&mut self, token: u64) -> bool {
        let Some(pending) = self.callback_pending.remove(&token) else {
            return false;
        };
        if pending.batch.is_some() {
            self.operator = None;
            self.operator_count = 1;
            self.count = None;
            self.pending = Pending::None;
            if self.mode == Mode::OperatorPending {
                self.mode = Mode::Normal;
            }
        }
        true
    }
    pub fn resume_callback(
        &mut self,
        token: u64,
        response: CallbackResponse,
    ) -> Result<EditorAction, EditorError> {
        let mut pending = self
            .callback_pending
            .remove(&token)
            .ok_or_else(|| EditorError("unknown or already completed callback token".into()))?;
        let mapping = matches!(pending.request.kind, CallbackKind::Mapping { .. });
        if !mapping && self.text.mark(self.text.len()).as_ref() != Some(&pending.owner) {
            return Err(EditorError("callback document changed".into()));
        }
        if let Some(batch) = pending.batch.take() {
            if self.selection_by_id(pending.selection).is_none() {
                return self.issue_callback_batch(*batch);
            }
            return self.resume_callback_batch(
                *batch,
                pending.selection,
                pending.request.kind,
                response,
            );
        }
        if !mapping {
            if self.selection_by_id(pending.selection).is_none() {
                return Ok(EditorAction::Continue);
            }
            self.set_primary_selection(pending.selection)?;
        }
        match (pending.request.kind, response) {
            (CallbackKind::Mapping { keys }, CallbackResponse::Mapping(consumed)) => {
                if consumed.is_some_and(|n| n < 0) {
                    self.callback_wait = Some((pending.request.callback, keys));
                    return Ok(EditorAction::Continue);
                }
                let n = consumed
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= keys.len() && keys.is_char_boundary(*n))
                    .unwrap_or(0);
                let mut consumed = 0;
                let mut remove = BTreeSet::new();
                for (id, len) in pending.queued {
                    if consumed + len > n {
                        break;
                    }
                    consumed += len;
                    remove.insert(id);
                }
                self.literal_queued.retain(|id| !remove.contains(id));
                self.recorded_queued.retain(|id| !remove.contains(id));
                self.queued_keys.retain(|(id, _)| !remove.contains(id));
                for queue in &mut self.queue_frames {
                    queue.retain(|(id, _)| !remove.contains(id));
                }
                Ok(EditorAction::Continue)
            }
            (CallbackKind::Motion { .. }, CallbackResponse::Position(Some(pos))) => {
                if pos > self.text.len() {
                    return Err(EditorError("callback position exceeds text length".into()));
                }
                let action = self.finish_motion(pos, false, false)?;
                self.sync_primary_to_set()?;
                Ok(action)
            }
            (CallbackKind::Operator { .. }, CallbackResponse::Position(pos)) => {
                self.mode = Mode::Normal;
                self.operator = None;
                self.count = None;
                if let Some(pos) = pos {
                    if pos > self.text.len() {
                        return Err(EditorError("callback position exceeds text length".into()));
                    }
                    self.update_selection(
                        pending.selection,
                        Selection {
                            anchor: pos,
                            cursor: pos,
                        },
                    )?;
                }
                self.text.snapshot();
                Ok(EditorAction::Continue)
            }
            (CallbackKind::TextObject { .. }, CallbackResponse::Range(Some(range))) => {
                if range.start > range.end || range.end > self.text.len() {
                    return Err(EditorError("invalid callback text object range".into()));
                }
                if self.operator.is_some() {
                    self.apply_operator(range, false)
                } else {
                    self.mode = Mode::Visual;
                    self.adopt_ranges(vec![range])?;
                    self.anchored.insert(pending.selection, true);
                    Ok(EditorAction::Continue)
                }
            }
            (CallbackKind::Motion { .. }, CallbackResponse::Position(None))
            | (CallbackKind::TextObject { .. }, CallbackResponse::Range(None)) => {
                Ok(EditorAction::Continue)
            }
            _ => Err(EditorError("callback response type mismatch".into())),
        }
    }
    pub fn current_count(&self) -> Option<usize> {
        self.count
    }
    pub fn set_count(&mut self, count: Option<usize>) -> Result<(), EditorError> {
        if count.is_some_and(|n| n > MAX_COUNT) {
            return Err(EditorError("count limit exceeded".into()));
        }
        self.count = count;
        Ok(())
    }
    pub fn switch_mode(&mut self, mode: Mode) -> Result<(), EditorError> {
        if mode == self.mode {
            return Ok(());
        }
        if mode == Mode::Command {
            return Err(EditorError("command mode needs a prompt".into()));
        }
        let previous = self.mode;
        if matches!(previous, Mode::Insert | Mode::Replace) {
            self.text.snapshot();
        }
        if matches!(previous, Mode::Visual | Mode::VisualLine)
            && !matches!(mode, Mode::Visual | Mode::VisualLine)
            && self.operator.is_none()
        {
            self.window_selection_save()?;
        }
        self.mode = mode;
        if matches!(mode, Mode::Insert | Mode::Replace) {
            self.enter_insert(mode == Mode::Replace, 1);
        }
        if matches!(mode, Mode::Visual | Mode::VisualLine) {
            self.anchor = self.cursor;
        }
        if mode == Mode::Normal {
            self.pending = Pending::None;
            self.operator = None;
            self.count = None;
        }
        let flag = self.is_visual();
        for id in self.selection_ids().to_vec() {
            self.anchored.insert(id, flag);
        }
        if !flag {
            self.selection_set
                .collapse_all(&self.text.to_vec())
                .map_err(|e| EditorError(e.to_string()))?;
            self.sync_set_to_primary();
        }
        self.sync_primary_to_set()?;
        self.materialize_visual_lines()
    }
    /// Native `window_selection_save`: store the last visual selection and add
    /// the current selection to the jump list.
    fn window_selection_save(&mut self) -> Result<(), EditorError> {
        self.save_selection_mark('^')?;
        self.jumplist_save();
        Ok(())
    }

    /// Native `view_selections_get_all`: ordered selection geometry using the
    /// cursor character, independent of whether visual mode is active.
    fn jump_ranges(&self) -> Vec<Range<usize>> {
        let data = self.text.to_vec();
        self.selection_set
            .selections()
            .iter()
            .map(|selection| {
                selection.anchor.min(selection.cursor)
                    ..motion::char_next(&data, selection.anchor.max(selection.cursor))
            })
            .collect()
    }

    /// Native `vis_jumplist(vis, 0)`.
    pub fn jumplist_save(&mut self) {
        let ranges = self.jump_ranges();
        self.jumplist.save(&self.text, &ranges, self.mode);
    }

    pub fn take_jumplist(&mut self) -> JumpList<Mode> {
        std::mem::take(&mut self.jumplist)
    }

    pub fn install_jumplist(&mut self, list: JumpList<Mode>) {
        self.jumplist = list;
    }

    /// Install the focused window's measured body geometry. `None` restores
    /// the native no-terminal defaults (80 columns, 23 body rows).
    pub fn set_viewport(&mut self, viewport: Option<crate::view::ViewportMetrics>) {
        self.viewport = viewport.unwrap_or(crate::view::ViewportMetrics {
            width: 80,
            height: 23,
            top: 0,
            wrapcolumn: 0,
        });
    }

    pub fn viewport(&self) -> crate::view::ViewportMetrics {
        self.viewport
    }

    /// Ask the next frame to start at a given display row. The request is
    /// one-shot: the workspace clears it after rendering.
    pub fn request_scroll(&mut self, top: usize) {
        self.scroll_request = Some(top);
    }

    /// Row the anchored first display line occupies in the current text.
    pub fn view_anchor_row(&self) -> Option<usize> {
        let mark = self.view_anchor.as_ref()?;
        let byte = self.text.mark_get(mark.clone())?;
        let data = self.text.to_vec();
        let viewport = self.viewport;
        let (row, _) = crate::view::screen_position_with_layout(
            &data,
            byte,
            viewport.width,
            viewport.wrap_width(),
            self.tab_width,
            &self.breakat,
        );
        Some(row)
    }

    /// Anchor the first display line at a row of the current text.
    pub fn set_view_anchor(&mut self, row: usize) {
        let data = self.text.to_vec();
        let columns = self.viewport.wrap_width();
        let Some(byte) =
            crate::view::row_start_with_breakat(&data, row, columns, self.tab_width, &self.breakat)
        else {
            self.view_anchor = None;
            return;
        };
        self.view_anchor = self.text.mark(byte);
    }

    /// Move the anchor out of the editor, for a window that loses the focus.
    pub fn take_view_anchor(&mut self) -> Option<crate::text::Mark> {
        self.view_anchor.take()
    }

    /// Restore the anchor of a window that gains the focus.
    pub fn install_view_anchor(&mut self, anchor: Option<crate::text::Mark>) {
        self.view_anchor = anchor;
    }

    pub fn scroll_request(&self) -> Option<usize> {
        self.scroll_request
    }

    pub fn clear_scroll_request(&mut self) {
        self.scroll_request = None;
    }

    /// Native `view_cursors_to`: a cursor that leaves the visible body is
    /// centered on the page, while native line motions scroll the view one
    /// row at a time and keep the cursor on the last or first visible row.
    fn request_centered_follow(&mut self) {
        let viewport = self.viewport;
        let columns = viewport.wrap_width();
        let height = viewport.height.max(1);
        let top = viewport.top;
        let data = self.text.to_vec();
        let (row, _) = crate::view::screen_position_with_layout(
            &data,
            self.cursor,
            viewport.width,
            columns,
            self.tab_width,
            &self.breakat,
        );
        if row < top || row >= top.saturating_add(height) {
            self.request_scroll(row.saturating_sub(height / 2));
        }
    }

    /// Native `vis_jumplist(vis, advance)`: save, or move once through the ring
    /// and apply the stored mode and geometry. The mode switch runs before the
    /// geometry is applied because leaving visual mode saves its own entry.
    pub fn jumplist_navigate(&mut self, advance: isize) -> Result<EditorAction, EditorError> {
        let current = self.jump_ranges();
        let target = match advance.signum() {
            0 => {
                self.jumplist_save();
                return Ok(EditorAction::Continue);
            }
            -1 => self.jumplist.older(&self.text, &current),
            _ => self.jumplist.newer(&self.text, &current),
        };
        let Some(target) = target else {
            return Ok(EditorAction::Continue);
        };
        if target.mode != self.mode && target.mode != Mode::Command {
            self.switch_mode(target.mode)?;
        }
        self.apply_jump_ranges(&target.ranges)?;
        Ok(EditorAction::Continue)
    }

    /// Native `view_selections_set_all`: reuse existing selections in order,
    /// preserve each one's orientation, create extra selections forward, set
    /// the primary to the first selection and never normalize geometry.
    fn apply_jump_ranges(&mut self, ranges: &[Range<usize>]) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let ids = self.selection_ids().to_vec();
        let existing = self.selection_set.selections().to_vec();
        let primary_id = self.selection_set.primary_id();
        let anchored = self.selection_anchored(primary_id).unwrap_or(false);
        let mut identified = Vec::new();
        if ranges.is_empty() {
            // Native empty application keeps only the primary, collapsed, and
            // leaves its cursor where it was.
            let selection = self.selection_set.primary();
            identified.push((
                Some(primary_id),
                Selection {
                    anchor: selection.cursor,
                    cursor: selection.cursor,
                },
            ));
        } else {
            for (index, range) in ranges.iter().enumerate() {
                let start = range.start.min(data.len());
                let mut end = range.end.min(data.len());
                if start != end {
                    end = motion::char_prev(&data, end);
                }
                let left_extending = existing
                    .get(index)
                    .is_some_and(|selection| selection.anchor > selection.cursor);
                identified.push((
                    ids.get(index).copied(),
                    if left_extending {
                        Selection {
                            anchor: end,
                            cursor: start,
                        }
                    } else {
                        Selection {
                            anchor: start,
                            cursor: end,
                        }
                    },
                ));
            }
        }
        let mut used = Vec::new();
        for (id, selection) in identified {
            let id = match id {
                Some(id) => id,
                None => SelectionId::fresh().map_err(|e| EditorError(e.to_string()))?,
            };
            used.push((id, selection));
        }
        let primary = used[0].0;
        self.selection_set = SelectionSet::from_identified(&data, used, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.external_marks.clear();
        let ids = self.selection_ids().to_vec();
        self.anchored.clear();
        for id in ids {
            self.anchored.insert(id, anchored);
        }
        self.sync_set_to_primary();
        Ok(())
    }

    /// Native `ka_selections_case`: map every selection's current character
    /// range in place. Same-length replacements keep all selection geometry;
    /// selections with invalid UTF-8 or a length-changing mapping are skipped,
    /// as C skips failed `mbstowcs`/`wcstombs` conversions.
    pub fn selection_case(&mut self, fold: CaseFold) -> Result<EditorAction, EditorError> {
        let data = self.text.to_vec();
        let mut edits = Vec::new();
        for range in self.jump_ranges() {
            if range.is_empty() || range.start >= data.len() {
                continue;
            }
            let Ok(slice) = std::str::from_utf8(&data[range.clone()]) else {
                continue;
            };
            let changed: String = slice.chars().map(|c| fold.apply(c)).collect();
            if changed.as_bytes() != &data[range.clone()] {
                edits.push((range, changed.into_bytes()));
            }
        }
        if edits.is_empty() {
            return Ok(EditorAction::Continue);
        }
        self.text.snapshot();
        for (range, bytes) in edits.into_iter().rev() {
            self.text.replace(range, &bytes)?;
        }
        self.recording_change = true;
        Ok(EditorAction::Continue)
    }

    pub fn get_register_slots(&self, name: char) -> Vec<Vec<u8>> {
        self.register_slots
            .get(&name.to_ascii_lowercase())
            .map(|slots| slots.iter().map(|r| r.bytes.clone()).collect())
            .unwrap_or_else(|| {
                self.register(name)
                    .map(|r| vec![r.bytes.clone()])
                    .unwrap_or_default()
            })
    }
    pub fn set_register_slots(
        &mut self,
        name: char,
        slots: Vec<Vec<u8>>,
    ) -> Result<(), EditorError> {
        if slots
            .iter()
            .try_fold(0usize, |n, v| n.checked_add(v.len()))
            .is_none_or(|n| n > MAX_INSERT_BYTES)
        {
            return Err(EditorError("register size limit exceeded".into()));
        }
        if name == '_' {
            return Ok(());
        }
        let name = name.to_ascii_lowercase();
        let slots: Vec<_> = slots
            .into_iter()
            .map(|bytes| Register {
                bytes,
                linewise: false,
            })
            .collect();
        self.registers
            .insert(name, slots.first().cloned().unwrap_or_default());
        self.register_slots.insert(name, slots);
        Ok(())
    }
    pub fn mark_names(&self) -> Vec<char> {
        self.buffer_state.saved.keys().copied().collect()
    }
    pub fn recording(&self) -> bool {
        self.macro_record.is_some()
    }
    pub fn input_queue(&self) -> String {
        self.queued_keys
            .iter()
            .map(|(_, key)| self.symbolic_key(*key))
            .collect()
    }
    pub fn selected_register(&self) -> Option<char> {
        self.selected_register
    }
    pub fn set_selected_register(&mut self, name: Option<char>) {
        self.selected_register = name;
    }
    pub fn selected_mark(&self) -> char {
        self.selected_mark
    }
    pub fn set_selected_mark(&mut self, name: char) {
        self.selected_mark = name;
    }
    pub fn insert_raw(&mut self, bytes: &[u8], replace: bool) -> Result<(), EditorError> {
        if bytes.len() > MAX_INSERT_BYTES {
            return Err(EditorError("insertion size limit exceeded".into()));
        }
        if !replace {
            self.insert_literal_bytes(bytes)?;
            return self.sync_primary_to_set();
        }
        let mut chars = 0;
        let mut at = 0;
        while at < bytes.len() {
            chars += 1;
            let next = motion::char_next(bytes, at);
            if next <= at {
                break;
            }
            at = next;
        }
        let data = self.text.to_vec();
        let edits = self
            .selections()
            .iter()
            .map(|s| {
                let mut end = s.cursor;
                for _ in 0..chars {
                    if end >= data.len() || data[end] == b'\n' {
                        break;
                    }
                    end = motion::char_next(&data, end);
                }
                (s.cursor..end, bytes.to_vec())
            })
            .collect();
        self.commit_multiple(edits, Placement::End)
    }
    pub fn register(&self, name: char) -> Option<&Register> {
        self.registers.get(&name.to_ascii_lowercase())
    }

    pub fn prompt_prefix(&self) -> char {
        match self.prompt {
            Prompt::Command => ':',
            Prompt::Search { backwards: false } => '/',
            Prompt::Search { backwards: true } => '?',
        }
    }

    pub fn selection(&self) -> Option<Range<usize>> {
        if !self.is_visual() {
            return None;
        }
        let data = self.text.to_vec();
        let a = self.anchor.min(data.len());
        let b = self.cursor.min(data.len());
        Some(a.min(b)..motion::char_next(&data, a.max(b)))
    }

    /// All active cursor/anchor pairs, in document order. The cursor field on
    /// Editor mirrors the primary pair for hosts which only need one caret.
    pub fn selections(&self) -> &[Selection] {
        self.selection_set.selections()
    }

    pub fn selection_ids(&self) -> &[SelectionId] {
        self.selection_set.ids()
    }
    pub fn view_identified(&self) -> Vec<(SelectionId, Selection)> {
        self.selection_set.identified()
    }
    pub fn selection_by_id(&self, id: SelectionId) -> Option<Selection> {
        self.selection_set.get(id)
    }
    pub fn update_selection(
        &mut self,
        id: SelectionId,
        selection: Selection,
    ) -> Result<(), EditorError> {
        self.selection_set
            .update(&self.text.to_vec(), id, selection)
            .map_err(|e| EditorError(e.to_string()))?;
        self.external_marks.remove(&id);
        self.sync_set_to_primary();
        Ok(())
    }
    pub fn create_selection(
        &mut self,
        selection: Selection,
        primary: bool,
    ) -> Result<SelectionId, EditorError> {
        let id = self
            .selection_set
            .push(&self.text.to_vec(), selection, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        Ok(id)
    }
    pub fn remove_selection(&mut self, id: SelectionId) -> Result<bool, EditorError> {
        let removed = self
            .selection_set
            .remove(id)
            .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        Ok(removed)
    }
    pub fn set_primary_selection(&mut self, id: SelectionId) -> Result<(), EditorError> {
        self.selection_set
            .set_primary_id(id)
            .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        Ok(())
    }
    pub fn selection_anchored(&self, id: SelectionId) -> Option<bool> {
        self.selection_by_id(id).map(|_| {
            self.anchored
                .get(&id)
                .copied()
                .unwrap_or_else(|| self.is_visual())
        })
    }
    pub fn set_selection_anchored(
        &mut self,
        id: SelectionId,
        anchored: bool,
    ) -> Result<(), EditorError> {
        if self.selection_by_id(id).is_none() {
            return Err(EditorError("unknown selection identity".into()));
        }
        self.anchored.insert(id, anchored);
        Ok(())
    }
    pub fn view_state(&self) -> Vec<(SelectionId, Selection, bool)> {
        self.view_identified()
            .into_iter()
            .map(|(id, s)| (id, s, self.selection_anchored(id).unwrap_or(false)))
            .collect()
    }
    pub fn restore_view_with_ids(
        &mut self,
        selections: Vec<(SelectionId, Selection)>,
        primary: SelectionId,
        mode: Mode,
    ) -> Result<(), EditorError> {
        let set = SelectionSet::from_identified(&self.text.to_vec(), selections, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.restore_view(set.selections().to_vec(), set.primary_index(), mode)?;
        self.selection_set = set;
        self.sync_set_to_primary();
        Ok(())
    }
    pub fn restore_view_state(
        &mut self,
        selections: Vec<(SelectionId, Selection, bool)>,
        primary: SelectionId,
        mode: Mode,
    ) -> Result<(), EditorError> {
        let flags = selections
            .iter()
            .map(|(id, _, flag)| (*id, *flag))
            .collect();
        self.restore_view_with_ids(
            selections.into_iter().map(|(id, s, _)| (id, s)).collect(),
            primary,
            mode,
        )?;
        self.anchored = flags;
        Ok(())
    }
    pub fn selection_range(&self, id: SelectionId) -> Option<Range<usize>> {
        let selection = self.selection_by_id(id)?;
        let selection = if let Some(marks) = self.external_marks.get(&id) {
            marks.restore(&self.text)?
        } else {
            selection
        };
        selection.range(&self.text.to_vec()).ok()
    }
    pub fn named_mark_get(&self, name: char) -> Vec<Range<usize>> {
        self.buffer_state.marked_ranges(&self.text, name)
    }
    pub fn named_mark_set(
        &mut self,
        name: char,
        ranges: Vec<Range<usize>>,
    ) -> Result<(), EditorError> {
        self.buffer_state
            .set_marked_ranges(&self.text, name, ranges)?;
        self.sync_named_marks();
        Ok(())
    }
    fn sync_named_marks(&mut self) {
        for (name, marks) in &self.buffer_state.saved {
            let positions = marks
                .iter()
                .filter_map(|m| m.restore(&self.text))
                .filter_map(|s| self.text.mark(s.cursor))
                .collect();
            let _ = self.session.set_marks(&self.text, *name, positions);
        }
    }
    /// Detach a completed ':' prompt for host command/plugin dispatch. Its
    /// original selection ranges remain available to execute_command.
    pub fn take_prompt_command(&mut self) -> Option<String> {
        if self.mode != Mode::Command || !matches!(self.prompt, Prompt::Command) {
            return None;
        }
        self.mode = Mode::Normal;
        self.pending = Pending::None;
        let command = std::mem::take(&mut self.command);
        if !command.is_empty() {
            self.history.entry(':').or_default().push(command.clone());
        }
        Some(command)
    }
    pub fn selection_marks(&self, id: SelectionId) -> Option<SelectionMarks> {
        self.external_marks.get(&id).cloned().or_else(|| {
            self.selection_by_id(id)
                .and_then(|s| s.save(&self.text).ok())
        })
    }
    pub fn restore_selection_marks(
        &mut self,
        marks: Vec<(SelectionId, SelectionMarks)>,
    ) -> Result<(), EditorError> {
        for (id, marks) in &marks {
            if self.selection_by_id(*id).is_none() || !marks.belongs_to(&self.text) {
                return Err(EditorError(
                    "selection marks belong to a different object or document".into(),
                ));
            }
        }
        self.external_marks = marks.into_iter().collect();
        for (id, _) in self.view_identified() {
            if let Some(marks) = self.external_marks.get(&id) {
                if let Some(restored) = marks.restore(&self.text) {
                    self.selection_set
                        .update(&self.text.to_vec(), id, restored)
                        .map_err(|e| EditorError(e.to_string()))?;
                }
            }
        }
        self.sync_set_to_primary();
        Ok(())
    }
    pub fn selection_position_valid(&self, id: SelectionId) -> bool {
        self.selection_position(id).is_some()
    }
    pub fn selection_position(&self, id: SelectionId) -> Option<usize> {
        let selection = self.selection_by_id(id)?;
        match self.external_marks.get(&id) {
            Some(marks) => marks.cursor_position(&self.text),
            None => Some(selection.cursor),
        }
    }
    /// Preserve live selection marks while a reentrant host callback edits bytes.
    pub fn mutate_text<R>(&mut self, f: impl FnOnce(&mut Text) -> R) -> R {
        self.with_sam_text_session(|text, _| f(text))
    }
    pub(crate) fn with_sam_text_session<R>(
        &mut self,
        f: impl FnOnce(&mut Text, &mut sam::Session) -> R,
    ) -> R {
        for (id, s) in self.view_identified() {
            if let Ok(mark) = s.save(&self.text) {
                self.external_marks.entry(id).or_insert(mark);
            }
        }
        let value = f(&mut self.text, &mut self.session);
        for (id, s) in self.view_identified() {
            let next = if let Some(marks) = self.external_marks.get(&id) {
                Selection {
                    anchor: marks
                        .anchor_position(&self.text)
                        .unwrap_or(s.anchor.min(self.text.len())),
                    cursor: marks
                        .cursor_position(&self.text)
                        .unwrap_or(s.cursor.min(self.text.len())),
                }
            } else {
                s
            };
            let _ = self.selection_set.update(&self.text.to_vec(), id, next);
        }
        self.sync_set_to_primary();
        value
    }
    pub fn primary_index(&self) -> usize {
        self.selection_set.primary_index()
    }

    pub fn swap_buffer_state(&mut self, state: &mut BufferState) {
        std::mem::swap(&mut self.buffer_state, state);
        self.sync_named_marks();
    }

    pub(crate) fn sam_session_mut(&mut self) -> &mut sam::Session {
        &mut self.session
    }
    pub(crate) fn external_command_names(&self) -> Vec<String> {
        self.external_commands.iter().cloned().collect()
    }
    pub(crate) fn sync_sam_registers(&mut self) {
        if let Some(pattern) = self.session.previous_regex() {
            self.search = pattern.to_owned();
            self.registers.insert(
                '/',
                Register {
                    bytes: pattern.as_bytes().to_vec(),
                    linewise: false,
                },
            );
        }
        if let Some(command) = self.session.previous_shell() {
            self.registers.insert(
                '!',
                Register {
                    bytes: command.as_bytes().to_vec(),
                    linewise: false,
                },
            );
        }
    }

    /// Native `shell` option: the program used for external commands.
    pub fn set_shell(&mut self, shell: Option<String>) {
        self.session.set_shell(shell);
    }

    /// The program used for external commands.
    pub fn shell(&self) -> String {
        self.session.shell_path().to_string_lossy().into_owned()
    }

    /// Native `escdelay` option: milliseconds to wait before an `Escape`
    /// sequence is read as a plain escape key.
    pub fn set_escape_delay(&mut self, delay: Option<std::time::Duration>) {
        self.escape_delay = delay;
    }

    /// Native `ignorecase` option: the search matches the ASCII letters
    /// regardless of case.
    pub fn set_ignorecase(&mut self, ignorecase: bool) {
        self.ignorecase = ignorecase;
    }

    /// Native `loadmethod` option: how a file is read.
    pub fn set_loadmethod(&mut self, loadmethod: Option<String>) {
        self.loadmethod = loadmethod;
    }

    pub fn loadmethod(&self) -> Option<String> {
        self.loadmethod.clone()
    }

    /// Native `change256colors` option: use the alternative 256 color
    /// palette.
    pub fn set_change256colors(&mut self, change256colors: bool) {
        self.change256colors = change256colors;
    }

    pub fn change256colors(&self) -> bool {
        self.change256colors
    }

    /// Native `breakat` option: the characters that may end a line, so that a
    /// word is kept together when it does not fit.
    pub fn set_breakat(&mut self, breakat: Option<String>) {
        self.breakat = std::sync::Arc::from(breakat.unwrap_or_default());
    }

    pub fn breakat(&self) -> &str {
        &self.breakat
    }

    pub fn ignorecase(&self) -> bool {
        self.ignorecase
    }

    /// The search pattern, compiled with the native `ignorecase` option.
    fn search_regex(&self, pattern: &str) -> Result<Regex, EditorError> {
        let regex = if self.ignorecase {
            Regex::new_ignorecase(pattern)
        } else {
            Regex::new(pattern)
        };
        regex.map_err(|error| EditorError(error.to_string()))
    }

    pub fn escape_delay(&self) -> Option<std::time::Duration> {
        self.escape_delay
    }

    pub fn set_directory(&mut self, path: &std::path::Path) {
        self.session.set_directory(path.to_path_buf());
    }

    pub fn forget_buffer(&mut self, text: &Text) {
        self.session.forget_document(text);
    }

    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    pub fn set_interrupt(&mut self, flag: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        self.session.set_interrupt(flag);
    }

    #[cfg(all(unix, any(feature = "terminal", feature = "lua")))]
    pub fn set_process_group_observer(
        &mut self,
        observer: std::sync::Arc<dyn Fn(Option<u32>) -> std::io::Result<()> + Send + Sync>,
    ) {
        self.session.set_process_group_observer(observer);
    }

    /// Restore a host window's view after its buffer has been installed. Global
    /// registers and command history remain in this editor; incomplete parser
    /// state belongs to the old view and is discarded.
    pub fn restore_view(
        &mut self,
        selections: Vec<Selection>,
        primary_index: usize,
        mode: Mode,
    ) -> Result<(), EditorError> {
        if !matches!(mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
            return Err(EditorError(
                "a restored view must be normal or visual".into(),
            ));
        }
        let data = self.text.to_vec();
        let set = SelectionSet::new(&data, selections, primary_index)
            .map_err(|e| EditorError(e.to_string()))?;
        self.selection_set = set;
        self.mode = mode;
        self.sync_set_to_primary();
        self.pending = Pending::None;
        self.operator = None;
        self.operator_count = 1;
        self.count = None;
        self.command.clear();
        self.command_keys.clear();
        self.motion_targets.clear();
        self.prompt_range = None;
        self.prompt_ranges.clear();
        self.selected_register = None;
        self.selected_mark = '\'';
        self.mapping_keys.clear();
        self.last_insert_newline = false;
        Ok(())
    }

    pub fn selection_ranges(&self) -> Vec<Range<usize>> {
        let prompt_visual = self.mode == Mode::Command
            && matches!(self.prompt_mode, Mode::Visual | Mode::VisualLine);
        if !self.is_visual() && !prompt_visual {
            return Vec::new();
        }
        let data = self.text.to_vec();
        self.selection_set
            .selections()
            .iter()
            .map(|selection| {
                selection.anchor.min(selection.cursor)
                    ..motion::char_next(&data, selection.anchor.max(selection.cursor))
            })
            .collect()
    }

    /// Linewise visual mode stores its expanded geometry in the view. Merely
    /// focusing a different view never changes that view's existing selection.
    fn materialize_visual_lines(&mut self) -> Result<(), EditorError> {
        if self.mode != Mode::VisualLine || self.operator.is_some() {
            return Ok(());
        }
        let data = self.text.to_vec();
        let primary = self.primary_index();
        let mut selections = self.selections().to_vec();
        selections[primary] = Selection {
            anchor: self.anchor.min(data.len()),
            cursor: self.cursor.min(data.len()),
        };
        let selections = selections
            .into_iter()
            .map(|selection| {
                let range = selection.range(&data)?;
                Selection::from_range(
                    &data,
                    motion::range_linewise(&data, range),
                    selection.direction(),
                )
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EditorError(e.to_string()))?;
        self.selection_set
            .replace_selections(&data, selections, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        for id in self.selection_ids().to_vec() {
            self.anchored.insert(id, true);
        }
        self.external_marks.clear();
        self.sync_set_to_primary();
        Ok(())
    }

    fn sync_primary_to_set(&mut self) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let id = self.selection_set.primary_id();
        let current = self.selection_set.primary();
        let selection = if self.cursor == current.cursor && self.anchor == current.anchor {
            current
        } else {
            Selection {
                anchor: if self.selection_anchored(id).unwrap_or(false)
                    || self.mode == Mode::Command
                        && matches!(self.prompt_mode, Mode::Visual | Mode::VisualLine)
                {
                    self.anchor.min(data.len())
                } else {
                    self.cursor.min(data.len())
                },
                cursor: self.cursor.min(data.len()),
            }
        };
        if self.selection_set.primary() != selection {
            self.external_marks.remove(&id);
        }
        self.selection_set
            .update(&data, id, selection)
            .map_err(|e| EditorError(e.to_string()))?;
        Ok(())
    }

    fn sync_set_to_primary(&mut self) {
        let ids: BTreeSet<_> = self.selection_ids().iter().copied().collect();
        self.anchored.retain(|id, _| ids.contains(id));
        self.external_marks.retain(|id, _| ids.contains(id));
        let primary = self.selection_set.primary();
        self.cursor = primary.cursor;
        self.anchor = primary.anchor;
    }

    fn adopt_ranges(&mut self, ranges: Vec<Range<usize>>) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        self.selection_set
            .set_ranges(&data, ranges)
            .map_err(|e| EditorError(e.to_string()))?;
        let anchored = self.is_visual();
        for id in self.selection_ids().to_vec() {
            self.anchored.insert(id, anchored);
        }
        self.external_marks.clear();
        self.sync_set_to_primary();
        Ok(())
    }

    fn adopt_sam_ranges(
        &mut self,
        ranges: Vec<Range<usize>>,
        inputs: &[usize],
        detached: &[bool],
        original_ids: &[SelectionId],
    ) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let mut used = BTreeSet::new();
        let mut identified = Vec::new();
        for (index, range) in ranges.into_iter().enumerate() {
            let old = inputs
                .get(index)
                .and_then(|input| original_ids.get(*input))
                .copied();
            let id = if !detached.get(index).copied().unwrap_or(true) {
                old.filter(|id| used.insert(*id))
            } else {
                None
            };
            let id = match id {
                Some(id) => id,
                None => SelectionId::fresh().map_err(|e| EditorError(e.to_string()))?,
            };
            identified.push((
                id,
                Selection::from_range(
                    &data,
                    range,
                    self.selection_by_id(id)
                        .map_or(crate::selection::Direction::Forward, |s| s.direction()),
                )
                .map_err(|e| EditorError(e.to_string()))?,
            ));
        }
        if identified.is_empty() {
            return Ok(());
        }
        let primary = identified[0].0;
        self.selection_set = SelectionSet::from_identified(&data, identified, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.selection_set
            .normalize(&data)
            .map_err(|e| EditorError(e.to_string()))?;
        let anchored = self.is_visual();
        for id in self.selection_ids().to_vec() {
            self.anchored.insert(id, anchored);
        }
        self.external_marks.clear();
        self.sync_set_to_primary();
        Ok(())
    }
    fn save_selection_mark(&mut self, name: char) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let ranges = if self.is_visual() {
            self.selection_ranges()
        } else {
            self.selection_set
                .ranges(&data)
                .map_err(|e| EditorError(e.to_string()))?
        };
        let marks = ranges
            .into_iter()
            .map(|range| {
                crate::selection::Selection::from_range(
                    &data,
                    range,
                    crate::selection::Direction::Forward,
                )
                .and_then(|selection| selection.save(&self.text))
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EditorError(e.to_string()))?;
        self.buffer_state.saved.insert(name, marks);
        let marks = self
            .selection_set
            .selections()
            .iter()
            .filter_map(|selection| self.text.mark(selection.cursor))
            .collect();
        self.session
            .set_marks(&self.text, name, marks)
            .map_err(|e| EditorError(e.to_string()))?;
        Ok(())
    }

    fn saved_ranges(&self, name: char) -> Vec<Range<usize>> {
        let data = self.text.to_vec();
        self.buffer_state
            .saved
            .get(&name)
            .into_iter()
            .flatten()
            .filter_map(|marks| marks.restore(&self.text))
            .filter_map(|selection| selection.range(&data).ok())
            .collect()
    }

    fn restore_selection_mark(&mut self, name: char, visual: bool) -> Result<(), EditorError> {
        let ranges = self.saved_ranges(name);
        if ranges.is_empty() {
            return Ok(());
        }
        if visual {
            self.mode = Mode::Visual;
        }
        self.adopt_ranges(ranges)?;
        if !self.is_visual() {
            let data = self.text.to_vec();
            self.selection_set
                .collapse_all(&data)
                .map_err(|e| EditorError(e.to_string()))?;
            self.sync_set_to_primary();
        }
        Ok(())
    }

    fn set_cursors(&mut self, positions: Vec<usize>, primary: usize) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        self.selection_set
            .replace_selections(
                &data,
                positions
                    .into_iter()
                    .map(|cursor| Selection {
                        anchor: cursor,
                        cursor,
                    })
                    .collect(),
                primary,
            )
            .map_err(|e| EditorError(e.to_string()))?;
        self.external_marks.clear();
        self.sync_set_to_primary();
        Ok(())
    }

    /// Validate the complete edit transcript before changing text. Reverse
    /// application keeps every source range in the original coordinate system.
    fn commit_multiple(
        &mut self,
        edits: Vec<(Range<usize>, Vec<u8>)>,
        placement: Placement,
    ) -> Result<(), EditorError> {
        let primary = self.selection_set.primary_index();
        let mut edits: Vec<_> = edits.into_iter().enumerate().collect();
        edits.sort_by_key(|(_, (range, _))| (range.start, range.end));
        let mut previous_end = 0;
        let mut removed = 0usize;
        let mut added = 0usize;
        let mut results = Vec::new();
        for (order, (index, (range, bytes))) in edits.iter().enumerate() {
            if range.start > range.end
                || range.end > self.text.len()
                || order > 0 && range.start < previous_end
            {
                return Err(EditorError(
                    "overlapping or invalid multiple-selection edits".into(),
                ));
            }
            previous_end = range.end;
            let start = range.start - removed + added;
            let end = start
                .checked_add(bytes.len())
                .ok_or_else(|| EditorError("text size limit exceeded".into()))?;
            results.push((*index, start..end));
            removed += range.end - range.start;
            added = added
                .checked_add(bytes.len())
                .ok_or_else(|| EditorError("text size limit exceeded".into()))?;
        }
        self.text
            .len()
            .checked_sub(removed)
            .and_then(|n| n.checked_add(added))
            .ok_or_else(|| EditorError("text size limit exceeded".into()))?;
        for (_, (range, bytes)) in edits.into_iter().rev() {
            self.text.replace(range, &bytes)?;
        }
        results.sort_by_key(|(index, _)| *index);
        let data = self.text.to_vec();
        let selections = results
            .into_iter()
            .map(|(_, range)| {
                let cursor = match placement {
                    Placement::Start => range.start,
                    Placement::End => range.end,
                };
                Selection {
                    anchor: cursor,
                    cursor,
                }
            })
            .collect();
        self.selection_set
            .replace_selections(&data, selections, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.external_marks.clear();
        self.sync_set_to_primary();
        self.recording_change = true;
        Ok(())
    }

    fn line_cursors(&mut self, end: bool) -> Result<EditorAction, EditorError> {
        let data = self.text.to_vec();
        let ranges = self.selection_ranges();
        let mut positions = Vec::new();
        for range in ranges {
            let mut line = motion::line_begin(&data, range.start);
            loop {
                positions.push(if end {
                    motion::line_finish(&data, line)
                } else {
                    motion::line_start(&data, line)
                });
                let next = motion::line_next(&data, line);
                if next <= line || next >= range.end {
                    break;
                }
                line = next;
            }
        }
        self.mode = Mode::Normal;
        let primary = positions.len().saturating_sub(1);
        self.selection_set = SelectionSet::new(
            &data,
            positions
                .into_iter()
                .map(|cursor| Selection {
                    anchor: cursor,
                    cursor,
                })
                .collect(),
            primary,
        )
        .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        Ok(EditorAction::Continue)
    }

    fn align_cursors(&mut self) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let column = self
            .selections()
            .iter()
            .map(|s| self.display_column(&data, s.cursor))
            .min()
            .unwrap_or(0);
        let positions = self
            .selections()
            .iter()
            .map(|s| self.column_position(&data, motion::line_begin(&data, s.cursor), column))
            .collect();
        self.set_cursors(positions, self.primary_index())
    }

    fn insert_preserving_selections(
        &mut self,
        mut edits: Vec<(usize, Vec<u8>)>,
    ) -> Result<(), EditorError> {
        if edits.is_empty() {
            return Ok(());
        }
        let total = edits
            .iter()
            .try_fold(0usize, |n, (_, bytes)| n.checked_add(bytes.len()))
            .filter(|n| *n <= MAX_INSERT_BYTES)
            .ok_or_else(|| EditorError("alignment insertion limit exceeded".into()))?;
        if total == 0 {
            return Ok(());
        }
        for (pos, _) in &edits {
            if *pos > self.text.len() {
                return Err(EditorError("invalid alignment position".into()));
            }
        }
        let primary = self.primary_index();
        let marks = self
            .selections()
            .iter()
            .map(|s| s.save(&self.text))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| EditorError(e.to_string()))?;
        edits.sort_by_key(|(pos, _)| *pos);
        for (pos, bytes) in edits.into_iter().rev() {
            self.text.insert(pos, &bytes)?;
        }
        let selections = marks
            .iter()
            .filter_map(|marks| marks.restore(&self.text))
            .collect();
        self.selection_set
            .replace_selections(&self.text.to_vec(), selections, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        self.recording_change = true;
        Ok(())
    }

    fn align_indentation(&mut self, right: bool) -> Result<(), EditorError> {
        let columns = self
            .selection_set
            .column_count(&self.text.to_vec())
            .map_err(|e| EditorError(e.to_string()))?;
        let insert_mode = matches!(self.mode, Mode::Insert | Mode::Replace);
        if !insert_mode {
            self.text.snapshot();
        }
        for column in 0..columns {
            let data = self.text.to_vec();
            let mut per_line = BTreeMap::new();
            let mut items = Vec::new();
            for selection in self.selections() {
                let line = motion::line_begin(&data, selection.cursor);
                let ordinal = per_line.entry(line).or_insert(0usize);
                let current = *ordinal;
                *ordinal += 1;
                if current == column {
                    let range = selection
                        .range(&data)
                        .map_err(|e| EditorError(e.to_string()))?;
                    let position = if right { range.end } else { range.start };
                    items.push((range.start, motion::line_width_get(&data, position)));
                }
            }
            let max = items.iter().map(|(_, width)| *width).max().unwrap_or(0);
            let edits = items
                .into_iter()
                .filter(|(_, width)| *width < max)
                .map(|(position, width)| (position, vec![b' '; max - width]))
                .collect();
            self.insert_preserving_selections(edits)?;
        }
        if !insert_mode {
            self.text.snapshot();
        }
        Ok(())
    }

    fn select_matching(&mut self, all: bool, skip: bool) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let source_id = self.selection_set.primary_id();
        let source = self.selection_set.primary();
        let range = source
            .range(&data)
            .map_err(|e| EditorError(e.to_string()))?;
        if range.is_empty() {
            return Ok(());
        }
        if self.selection_set.len() == 1 {
            self.match_word = range == motion::word_object(&data, source.cursor, false, false);
        }
        let needle = &data[range.clone()];
        let mut matches = Vec::new();
        let mut pos = 0;
        while pos < data.len() {
            let Some(start) = motion::find_next(&data, pos, needle) else {
                break;
            };
            let found = if self.match_word {
                let word = motion::word_object(&data, start, false, false);
                if word.len() != needle.len() {
                    pos = word.end.max(start + 1);
                    continue;
                }
                word
            } else {
                start..start + needle.len()
            };
            pos = found.end.max(found.start + 1);
            matches.push(found);
        }
        let existing = self
            .selection_set
            .ranges(&data)
            .map_err(|e| EditorError(e.to_string()))?;
        let available: Vec<_> = matches
            .into_iter()
            .filter(|candidate| {
                !existing
                    .iter()
                    .any(|r| r.start < candidate.end && candidate.start < r.end)
            })
            .collect();
        if all {
            for range in available {
                let selection =
                    Selection::from_range(&data, range, crate::selection::Direction::Forward)
                        .map_err(|e| EditorError(e.to_string()))?;
                self.selection_set
                    .add(&data, selection, false)
                    .map_err(|e| EditorError(e.to_string()))?;
            }
        } else {
            let found = available
                .iter()
                .find(|candidate| candidate.start >= range.end)
                .or_else(|| {
                    available
                        .iter()
                        .rev()
                        .find(|candidate| candidate.end <= range.start)
                });
            if let Some(found) = found {
                let selection = Selection::from_range(
                    &data,
                    found.clone(),
                    crate::selection::Direction::Forward,
                )
                .map_err(|e| EditorError(e.to_string()))?;
                self.selection_set
                    .add(&data, selection, true)
                    .map_err(|e| EditorError(e.to_string()))?;
                if skip {
                    self.selection_set
                        .remove(source_id)
                        .map_err(|e| EditorError(e.to_string()))?;
                }
            }
        }
        self.sync_set_to_primary();
        Ok(())
    }

    fn configure_langmap(&mut self, args: &[String]) -> Result<(), EditorError> {
        let (Some(source), Some(target)) = (args.first(), args.get(1)) else {
            return Err(EditorError(
                "usage: langmap <non-latin keys> <latin keys>".into(),
            ));
        };
        let mut duplicate = false;
        for (source, target) in source.chars().zip(target.chars()) {
            match self.language_map.entry(source) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(target);
                }
                std::collections::btree_map::Entry::Occupied(_) => duplicate = true,
            }
        }
        if duplicate {
            Err(EditorError("language key mapping already exists".into()))
        } else {
            Ok(())
        }
    }
    fn translate_language_key(&mut self, key: Key) -> Key {
        if self.feeding_keys || self.mapping_expanding || self.replaying || self.macro_depth > 0 {
            return key;
        }
        let disabled = std::mem::take(&mut self.language_map_disabled);
        if disabled || matches!(self.mode, Mode::Insert | Mode::Replace | Mode::Command) {
            return key;
        }
        let translate = |c| self.language_map.get(&c).copied().unwrap_or(c);
        match key {
            Key::Char(c) => Key::Char(translate(c)),
            Key::Ctrl(c) => Key::Ctrl(translate(c)),
            Key::Alt(c) => Key::Alt(translate(c)),
            Key::AltCtrl(c) => Key::AltCtrl(translate(c)),
            _ => key,
        }
    }

    /// Process one decoded key. Errors cancel an incomplete operator and leave
    /// the state machine ready for the next command; text errors are propagated.
    pub fn handle_key(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        let key = self.translate_language_key(key);
        if matches!(key, Key::SelectionsSave | Key::SelectionsRestore) {
            let mark = std::mem::replace(&mut self.selected_mark, '\'');
            if key == Key::SelectionsSave {
                self.save_selection_mark(mark)?;
            } else {
                self.restore_selection_mark(mark, self.is_visual())?;
            }
            self.pending = Pending::None;
            self.operator = None;
            self.operator_count = 1;
            self.count = None;
            return Ok(EditorAction::Continue);
        }
        if self.mode == Mode::Normal
            && matches!(self.pending, Pending::None)
            && key == Key::Char('q')
            && self.macro_record.is_some()
            && self.macro_depth == 0
        {
            let name = self.macro_record.take().expect("checked recording");
            self.macro_keys.clear();
            self.last_macro = Some(name.to_ascii_lowercase());
            self.command_keys.clear();
            return Ok(EditorAction::Continue);
        }
        if let Some(name) = self
            .macro_record
            .filter(|_| self.macro_depth == 0 && !self.mapping_expanding && !self.feeding_keys)
        {
            if self.macro_keys.len() >= MAX_REPEAT_KEYS {
                return Err(EditorError("macro recording limit exceeded".into()));
            }
            self.macro_keys.push(key);
            let name = name.to_ascii_lowercase();
            self.registers
                .entry(name)
                .or_default()
                .bytes
                .extend_from_slice(Self::key_notation(key).as_bytes());
        }
        if !self.replaying && !self.suppress_command_record {
            if self.command_keys.len() >= MAX_REPEAT_KEYS {
                self.cancel();
                return Err(EditorError("command key limit exceeded".into()));
            }
            self.command_keys.push(key);
        }
        if let Some((id, _)) = self.callback_wait.take() {
            self.enqueue_keys(vec![key])?;
            self.recorded_queued.insert(self.queue_token);
            return self.request_callback(
                CallbackKind::Mapping {
                    keys: self.input_queue(),
                },
                id,
            );
        }
        if !self.mapping_expanding
            && !matches!(
                self.pending,
                Pending::VerbatimStart | Pending::Verbatim { .. } | Pending::InsertDigraph
            )
        {
            let mut candidate = std::mem::take(&mut self.mapping_keys);
            candidate.push(key);
            if let Some(mapping) = self
                .mappings
                .iter()
                .filter(|mapping| mapping.window.is_none() || mapping.window == self.window_id)
                .filter(|mapping| mapping.mode == self.mode && mapping.left == candidate)
                .max_by_key(|mapping| mapping.window.is_some())
            {
                let right = mapping.right.clone();
                if let Some(id) = mapping.callback {
                    return self.request_callback(
                        CallbackKind::Mapping {
                            keys: self.input_queue(),
                        },
                        id,
                    );
                }
                return self.expand_mapping(right);
            }
            if self.mappings.iter().any(|mapping| {
                (mapping.window.is_none() || mapping.window == self.window_id)
                    && mapping.mode == self.mode
                    && mapping.left.starts_with(&candidate)
            }) {
                self.mapping_keys = candidate;
                return Ok(EditorAction::Continue);
            }
            if candidate.len() > 1 {
                return self.expand_literal_keys(candidate);
            }
        }
        // The default Ctrl-M binding aliases Enter, so window-local Enter
        // mappings must resolve before the insertion/newline implementation.
        if key == Key::Ctrl('m')
            && matches!(self.pending, Pending::None)
            && self.mode != Mode::Command
        {
            return self.expand_mapping(vec![Key::Enter]);
        }
        self.cursor = self.cursor.min(self.text.len());
        self.sync_primary_to_set()?;
        if matches!(self.mode, Mode::Insert | Mode::Replace | Mode::Command)
            || !matches!(
                key,
                Key::Up
                    | Key::Down
                    | Key::Enter
                    | Key::Char('j' | 'k' | '0'..='9')
                    | Key::Native(_)
            )
        {
            self.preferred_column = None;
        }
        if self.mode == Mode::Normal
            && matches!(self.pending, Pending::None)
            && self.operator.is_none()
            && key == Key::Char('.')
        {
            let count = self.count.take();
            self.command_keys.clear();
            let result = self.repeat(count);
            self.sync_primary_to_set()?;
            return result;
        }
        if self.is_visual() && matches!(key, Key::Escape | Key::Ctrl('[')) && self.count.is_none() {
            self.window_selection_save()?;
        }
        let register_prefix = matches!(self.pending, Pending::Register);
        let before = self.text.len();
        let was_visual = self.is_visual();
        let result = self.process_key(key);
        if was_visual != self.is_visual() {
            let flag = self.is_visual();
            for id in self.selection_ids().to_vec() {
                self.anchored.insert(id, flag);
            }
        }
        if result.is_err() {
            self.cancel();
            self.command_keys.clear();
            self.recording_change = false;
        } else if !register_prefix
            && !matches!(&result, Ok(EditorAction::PromptShow { .. }))
            && self.mode == Mode::Normal
            && matches!(self.pending, Pending::None)
            && self.operator.is_none()
            && self.count.is_none()
        {
            if !self.internal_scope && !self.replaying && self.recording_change && self.repeatable {
                self.last_change = std::mem::take(&mut self.command_keys);
            }
            self.command_keys.clear();
            self.recording_change = false;
            self.repeatable = true;
            self.selected_register = None;
        }
        // The change flag also covers same-length replacements through helpers.
        if before != self.text.len() && !self.replaying && self.mode != Mode::Normal {
            self.recording_change = true;
        }
        self.sync_primary_to_set()?;
        result
    }

    fn cancel(&mut self) {
        let keep_primary = self.mode == Mode::Normal;
        let data = self.text.to_vec();
        // Escape first collapses visual ranges to their cursor endpoints; a
        // second Escape in normal mode removes all secondary cursors.
        if keep_primary {
            self.selection_set.keep_primary();
        }
        let _ = self.selection_set.collapse_all(&data);
        self.sync_set_to_primary();
        self.mode = Mode::Normal;
        self.pending = Pending::None;
        self.operator = None;
        self.operator_count = 1;
        self.count = None;
        self.selected_register = None;
        self.prompt_range = None;
        self.prompt_ranges.clear();
        self.text.snapshot();
    }

    /// Insert bracketed-paste bytes literally. Outside insert/replace mode this
    /// fails rather than interpreting pasted bytes as editor commands.
    pub fn paste(&mut self, bytes: &[u8]) -> Result<(), EditorError> {
        self.last_insert_newline = false;
        if !matches!(self.mode, Mode::Insert | Mode::Replace) {
            return Err(EditorError("paste requires insert or replace mode".into()));
        }
        if bytes.len() > MAX_INSERT_BYTES {
            return Err(EditorError("paste size limit exceeded".into()));
        }
        // A paste is one edit rather than a key replay: NUL, invalid UTF-8,
        // escapes, and newlines retain their literal byte values.
        if self.selection_set.len() > 1 {
            let edits = self
                .selection_set
                .selections()
                .iter()
                .map(|selection| (selection.cursor..selection.cursor, bytes.to_vec()))
                .collect();
            self.commit_multiple(edits, Placement::End)?;
        } else {
            self.text.insert(self.cursor, bytes)?;
            self.cursor += bytes.len();
        }
        self.recording_change |= !bytes.is_empty();
        // Byte pastes cannot be faithfully represented by Char keys. Avoid
        // recording an incomplete dot command which would replay different data.
        self.command_keys.clear();
        self.last_change.clear();
        self.recording_change = false;
        self.repeatable = false;
        self.sync_primary_to_set()?;
        Ok(())
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1)
    }

    fn digit(&mut self, c: char) -> Result<bool, EditorError> {
        if let Some(n) = c.to_digit(10) {
            if n != 0 || self.count.is_some() {
                let value = self
                    .count
                    .unwrap_or(0)
                    .checked_mul(10)
                    .and_then(|v| v.checked_add(n as usize))
                    .filter(|v| *v <= MAX_COUNT)
                    .ok_or_else(|| EditorError("count limit exceeded".into()))?;
                self.count = Some(value);
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn process_key(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        if matches!(self.pending, Pending::None) {
            if let Key::Native(action) = key {
                return self.invoke_native_action(action);
            }
            if let Key::Action(id) = key {
                return self.request_callback(
                    CallbackKind::Mapping {
                        keys: self.input_queue(),
                    },
                    id,
                );
            }
        }

        match key {
            Key::PromptShow | Key::PromptSearchForward | Key::PromptSearchBackward => {
                let prompt = match key {
                    Key::PromptSearchForward => Prompt::Search { backwards: false },
                    Key::PromptSearchBackward => Prompt::Search { backwards: true },
                    _ => Prompt::Command,
                };
                return self.show_prompt(prompt);
            }
            Key::PromptSubmit => return Ok(EditorAction::PromptSubmit),
            Key::PromptCancel => return Ok(EditorAction::PromptCancel),
            Key::PromptHistoryExpand => return Ok(EditorAction::PromptHistoryExpand),
            _ => {}
        }
        if self.mode == Mode::Command {
            return self.prompt_key(key);
        }
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            return self.insert_key(key);
        }
        if matches!(key, Key::Escape | Key::Ctrl('[')) {
            if self.is_visual() && self.count.take().is_some() {
                return Ok(EditorAction::Continue);
            }
            if matches!(self.pending, Pending::ReplaceChar) && self.is_visual() {
                self.pending = Pending::None;
                self.count = None;
                return Ok(EditorAction::Continue);
            }
            self.cancel();
            return Ok(EditorAction::Continue);
        }
        let pending = std::mem::replace(&mut self.pending, Pending::None);
        match pending {
            Pending::Register => {
                if let Key::Char(name) = key {
                    if name.is_ascii_alphanumeric() || matches!(name, '"' | '_' | '/' | ':') {
                        self.selected_register = Some(name);
                        // Keep register prefix as part of the following action.
                        self.pending = Pending::None;
                        return Ok(EditorAction::Continue);
                    }
                }
                return Err(EditorError("invalid register".into()));
            }
            Pending::ReplaceChar => return self.replace_character(key),
            Pending::Find {
                backwards,
                till,
                cross_line,
            } => {
                let character = match key {
                    Key::Char(c) => Some(c),
                    Key::Tab => Some('\t'),
                    Key::Enter => Some('\n'),
                    Key::Ctrl(c) if c.is_ascii() => Some(((c as u8) & 0x1f) as char),
                    _ => None,
                };
                if let Some(character) = character {
                    let find = Find {
                        character,
                        backwards,
                        till,
                        cross_line,
                    };
                    self.last_find = Some(find);
                    let count = self.combined_count()?;
                    let (target, inclusive) = self.find_targets(find, count);
                    return self.finish_motion(target, inclusive, false);
                }
                return Err(EditorError("character expected".into()));
            }
            Pending::TextObject { outer } => {
                if let Key::Char(c) = key {
                    return self.text_object(c, outer);
                }
                if key == Key::Tab {
                    return self.text_object('\t', outer);
                }
                return Err(EditorError("text object expected".into()));
            }
            Pending::G => return self.g_key(key),
            Pending::InsertSlide => {
                let action = match key {
                    Key::Ctrl('e') => Some(NativeAction::WindowSlideUp),
                    Key::Ctrl('y') => Some(NativeAction::WindowSlideDown),
                    _ => None,
                };
                let Some(action) = action else {
                    return Err(EditorError("unknown insert window command".into()));
                };
                return self.invoke_native_action(action);
            }
            Pending::Bracket { closing } => {
                let action = match (closing, key) {
                    (false, Key::Char('{')) => Some(NativeAction::CursorBlockStart),
                    (false, Key::Char('(')) => Some(NativeAction::CursorParenthesisStart),
                    (true, Key::Char('}')) => Some(NativeAction::CursorBlockEnd),
                    (true, Key::Char(')')) => Some(NativeAction::CursorParenthesisEnd),
                    _ => None,
                };
                let Some(action) = action else {
                    return Err(EditorError("unknown bracket motion".into()));
                };
                return self.invoke_native_action(action);
            }
            Pending::Z => {
                return match key {
                    Key::Char('t') => self.invoke_native_action(NativeAction::WindowRedrawTop),
                    Key::Char('z') => self.invoke_native_action(NativeAction::WindowRedrawCenter),
                    Key::Char('b') => self.invoke_native_action(NativeAction::WindowRedrawBottom),
                    Key::Char('Z') => Ok(EditorAction::Save {
                        quit: true,
                        force: false,
                        path: None,
                        ranges: None,
                        visual: false,
                    }),
                    Key::Char('Q') => Ok(EditorAction::Quit { force: true }),
                    _ => Err(EditorError("unknown Z command".into())),
                };
            }
            Pending::Window => {
                let command = match key {
                    Key::Char('w') | Key::Ctrl('w') => "w",
                    Key::Char('p') => "p",
                    Key::Char('h') => "h",
                    Key::Char('j') | Key::Down => "j",
                    Key::Char('k') | Key::Up => "k",
                    Key::Char('l') => "l",
                    Key::Char('c') => "c",
                    Key::Char('o') => "o",
                    Key::Char('s') => "s",
                    Key::Char('v') => "v",
                    _ => return Err(EditorError("unsupported window command".into())),
                };
                return Ok(EditorAction::HostCommand(format!("wincmd {command}")));
            }
            Pending::Mark => {
                if let Key::Char(name) = key {
                    if name.is_ascii_lowercase() || matches!(name, '\'' | '^') {
                        self.selected_mark = name;
                        return Ok(EditorAction::Continue);
                    }
                }
                return Err(EditorError("invalid mark name".into()));
            }
            Pending::None => {}
            Pending::MacroRecord => {
                if let Key::Char(name) = key {
                    if name.is_ascii_alphanumeric() {
                        self.macro_record = Some(name);
                        self.macro_keys.clear();
                        let name = name.to_ascii_lowercase();
                        if !matches!(key,Key::Char(c)if c.is_ascii_uppercase()) {
                            self.registers.entry(name).or_default().bytes.clear();
                        }
                        self.register_slots.remove(&name);
                        return Ok(EditorAction::Continue);
                    }
                }
                return Err(EditorError("macro register expected".into()));
            }
            Pending::MacroPlay => {
                let name = match key {
                    Key::Char('@') => self
                        .last_macro
                        .ok_or_else(|| EditorError("no previous macro".into()))?,
                    Key::Char(name) => name,
                    _ => return Err(EditorError("macro register expected".into())),
                };
                let count = self.take_count();
                return self.play_macro(name, count);
            }
            Pending::InsertRegister
            | Pending::InsertDigraph
            | Pending::VerbatimStart
            | Pending::Verbatim { .. } => unreachable!("insert-only pending state"),
        }
        let key = if key == Key::Ctrl('m') {
            Key::Enter
        } else {
            key
        };
        if let Key::Char(c) = key {
            if self.digit(c)? {
                return Ok(EditorAction::Continue);
            }
        }
        // Space has no binding in vis's normal/visual/operator maps. It is
        // frequently used as readable separation in the upstream key fixtures.
        if key == Key::Char(' ') {
            return Ok(EditorAction::Continue);
        }
        if let Some(operator) = self.operator {
            let doubled = matches!(
                (operator, key),
                (Operator::Delete, Key::Char('d'))
                    | (Operator::Change, Key::Char('c'))
                    | (Operator::Yank, Key::Char('y'))
                    | (Operator::ShiftLeft, Key::Char('<'))
                    | (Operator::ShiftRight, Key::Char('>'))
            );
            if doubled {
                let count = self.combined_count()?;
                let data = self.text.to_vec();
                if self.selection_set.len() > 1 {
                    let ranges = self
                        .selection_set
                        .selections()
                        .iter()
                        .map(|selection| {
                            let start = motion::line_begin(&data, selection.cursor);
                            let mut end = start;
                            for _ in 0..count {
                                end = motion::line_next(&data, end);
                            }
                            start..end
                        })
                        .collect();
                    return self.apply_many_operators(ranges, true);
                }
                let start = motion::line_begin(&data, self.cursor);
                let mut end = start;
                for _ in 0..count {
                    end = motion::line_next(&data, end);
                }
                return self.apply_operator(start..end, true);
            }
            if matches!(key, Key::Char('i' | 'a')) {
                self.pending = Pending::TextObject {
                    outer: key == Key::Char('a'),
                };
                return Ok(EditorAction::Continue);
            }
        }
        if self.is_visual() && matches!(key, Key::Char('i' | 'a')) {
            self.pending = Pending::TextObject {
                outer: key == Key::Char('a'),
            };
            return Ok(EditorAction::Continue);
        }
        let jump = Self::jump_motion_key(key) && !(key == Key::Char('%') && self.count.is_some());
        if let Some((target, inclusive, linewise, linewise_inclusive)) = self.motion(key)? {
            let action =
                self.finish_motion_native(target, inclusive, linewise, linewise_inclusive)?;
            if jump {
                self.jumplist_save();
            }
            if !matches!(key, Key::Char('j' | 'k') | Key::Up | Key::Down | Key::Enter) {
                self.request_centered_follow();
            }
            return Ok(action);
        }
        if self.operator.is_some()
            && !matches!(
                key,
                Key::Char(
                    'g' | 'f' | 'F' | 't' | 'T' | ';' | ',' | '"' | '/' | '?' | 'H' | 'M' | 'L'
                )
            )
        {
            return Err(EditorError(
                "operator requires a motion or text object".into(),
            ));
        }
        match key {
            Key::Char('"') => {
                self.pending = Pending::Register;
            }
            Key::Char('[') => self.pending = Pending::Bracket { closing: false },
            Key::Char(']') => self.pending = Pending::Bracket { closing: true },
            Key::Char('\'') => self.pending = Pending::Mark,
            Key::Char('m') => {
                let name = std::mem::replace(&mut self.selected_mark, '\'');
                self.save_selection_mark(name)?;
            }
            // The selections table precedes the motions table, so normal-mode
            // `M` restores the selection mark; window-middle is action-only.
            Key::Char('M') => {
                let name = std::mem::replace(&mut self.selected_mark, '\'');
                self.restore_selection_mark(name, false)?;
            }
            Key::Char('H') => return self.invoke_native_action(NativeAction::CursorWindowLineTop),
            Key::Char('L') => {
                return self.invoke_native_action(NativeAction::CursorWindowLineBottom);
            }
            Key::Char('|' | '&' | '\\' | '~') => {
                let data = self.text.to_vec();
                let name = std::mem::replace(&mut self.selected_mark, '\'');
                let saved = self.saved_ranges(name);
                match key {
                    Key::Char('|') => self.selection_set.union_with(&data, &saved),
                    Key::Char('&') => self.selection_set.intersect_with(&data, &saved),
                    Key::Char('\\') => self.selection_set.subtract(&data, &saved),
                    _ => self.selection_set.invert(&data),
                }
                .map_err(|e| EditorError(e.to_string()))?;
                if !self.is_visual() {
                    self.selection_set
                        .collapse_all(&data)
                        .map_err(|e| EditorError(e.to_string()))?;
                }
                self.sync_set_to_primary();
            }
            Key::Char('_') => {
                let data = self.text.to_vec();
                if !self
                    .selection_set
                    .trim(&data)
                    .map_err(|e| EditorError(e.to_string()))?
                {
                    self.mode = Mode::Normal;
                }
                self.sync_set_to_primary();
            }
            Key::Char('+' | '-') if self.is_visual() => {
                let count = self.take_count() as isize;
                self.selection_set
                    .rotate_contents(
                        &mut self.text,
                        if key == Key::Char('+') { count } else { -count },
                    )
                    .map_err(|e| EditorError(e.to_string()))?;
                self.sync_set_to_primary();
                self.recording_change = true;
            }
            Key::Char('g') => {
                self.pending = Pending::G;
            }
            Key::Char('f' | 'F' | 't' | 'T') => {
                if self.queued_keys.is_empty() {
                    self.language_map_disabled = true;
                }
                self.pending = Pending::Find {
                    backwards: matches!(key, Key::Char('F' | 'T')),
                    till: matches!(key, Key::Char('t' | 'T')),
                    cross_line: false,
                };
            }
            Key::Char(';' | ',') => {
                let Some(mut find) = self.last_find else {
                    // Native find-repeat actions consume this key without
                    // cancelling visual or operator-pending state when no
                    // previous character search exists.
                    return Ok(EditorAction::Continue);
                };
                if key == Key::Char(',') {
                    find.backwards = !find.backwards;
                }
                let count = self.take_count();
                let (target, inclusive) = self.find_targets(find, count);
                return self.finish_motion(target, inclusive, false);
            }
            Key::Char('d' | 'c' | 'y' | '<' | '>') => {
                let operator = match key {
                    Key::Char('d') => Operator::Delete,
                    Key::Char('c') => Operator::Change,
                    Key::Char('y') => Operator::Yank,
                    Key::Char('<') => Operator::ShiftLeft,
                    _ => Operator::ShiftRight,
                };
                self.operator = Some(operator);
                if let Some(range) = self.selection() {
                    let restore = matches!(operator, Operator::ShiftLeft | Operator::ShiftRight);
                    let action = self.apply_operator(range, self.mode == Mode::VisualLine)?;
                    if restore {
                        self.restore_selection_mark('^', true)?;
                    }
                    return Ok(action);
                }
                self.operator_count = 1;
                self.mode = Mode::OperatorPending;
            }
            Key::Char('i' | 'I' | 'a' | 'A' | 'o' | 'O' | 'R')
                if self.operator.is_none() && !self.is_visual() =>
            {
                let count = self.take_count();
                if self.selection_set.len() > 1 {
                    self.begin_multi_insert(key, count)?;
                    return Ok(EditorAction::Continue);
                }
                let data = self.text.to_vec();
                self.text.snapshot();
                match key {
                    Key::Char('I') => self.cursor = motion::line_start(&data, self.cursor),
                    Key::Char('a') => {
                        if data.get(self.cursor).is_some_and(|b| *b != b'\n') {
                            self.cursor = motion::char_next(&data, self.cursor);
                        }
                    }
                    Key::Char('A') => self.cursor = motion::line_end(&data, self.cursor),
                    Key::Char('o' | 'O') => {
                        let below = key == Key::Char('o');
                        let start = motion::line_begin(&data, self.cursor);
                        let indent = if self.auto_indent {
                            data[start..motion::line_start(&data, self.cursor)].to_vec()
                        } else {
                            Vec::new()
                        };
                        let pos = if below {
                            motion::line_next(&data, self.cursor)
                        } else {
                            start
                        };
                        let mut bytes = Vec::new();
                        if below && pos > 0 && data.get(pos - 1) != Some(&b'\n') {
                            bytes.push(b'\n');
                        }
                        let cursor = pos + bytes.len() + indent.len();
                        bytes.extend_from_slice(&indent);
                        bytes.push(b'\n');
                        self.text.insert(pos, &bytes)?;
                        self.cursor = cursor;
                        self.recording_change = true;
                    }
                    _ => {}
                }
                self.enter_insert(key == Key::Char('R'), count);
                self.open_line = match key {
                    Key::Char('o') => Some(true),
                    Key::Char('O') => Some(false),
                    _ => None,
                };
            }
            Key::Char('x' | 'X') | Key::Delete => {
                // The native visual table aliases `x` to the delete operator,
                // has no `X` binding, and runs the delete action for <Delete>,
                // whose motion then applies in the restored normal mode.
                if self.is_visual() {
                    if key == Key::Char('X') {
                        return Ok(EditorAction::Continue);
                    }
                    self.operator = Some(Operator::Delete);
                    let Some(range) = self.selection() else {
                        return Ok(EditorAction::Continue);
                    };
                    let action = self.apply_operator(range, self.mode == Mode::VisualLine)?;
                    if key == Key::Delete {
                        let data = self.text.to_vec();
                        self.cursor = motion::char_next(&data, self.cursor);
                    }
                    return Ok(action);
                }
                let count = self.take_count();
                self.operator = Some(Operator::Delete);
                let data = self.text.to_vec();
                if self.selection_set.len() > 1 {
                    let ranges = self
                        .selection_set
                        .selections()
                        .iter()
                        .map(|selection| {
                            let mut end = selection.cursor;
                            for _ in 0..count {
                                end = if key == Key::Char('X') {
                                    motion::char_prev(&data, end)
                                } else {
                                    motion::char_next(&data, end)
                                };
                            }
                            selection.cursor.min(end)..selection.cursor.max(end)
                        })
                        .collect();
                    return self.apply_many_operators(ranges, false);
                }
                let mut other = self.cursor;
                for _ in 0..count {
                    other = if key == Key::Char('X') {
                        motion::char_prev(&data, other)
                    } else {
                        motion::char_next(&data, other)
                    };
                }
                return self.apply_operator(self.cursor.min(other)..self.cursor.max(other), false);
            }
            Key::Char('D' | 'C' | 'Y') if !self.is_visual() => {
                let data = self.text.to_vec();
                self.operator = Some(match key {
                    Key::Char('D') => Operator::Delete,
                    Key::Char('C') => Operator::Change,
                    _ => Operator::Yank,
                });
                if self.selection_set.len() > 1 {
                    let ranges = self
                        .selection_set
                        .selections()
                        .iter()
                        .map(|selection| {
                            selection.cursor..motion::line_end(&data, selection.cursor)
                        })
                        .collect();
                    return self.apply_many_operators(ranges, false);
                }
                return self
                    .apply_operator(self.cursor..motion::line_end(&data, self.cursor), false);
            }
            Key::Char('s' | 'S') => {
                // The native visual table aliases `s` to the change operator
                // and has no `S` binding.
                if self.is_visual() {
                    if key == Key::Char('S') {
                        return Ok(EditorAction::Continue);
                    }
                    self.operator = Some(Operator::Change);
                    let Some(range) = self.selection() else {
                        return Ok(EditorAction::Continue);
                    };
                    return self.apply_operator(range, self.mode == Mode::VisualLine);
                }
                self.operator = Some(Operator::Change);
                let data = self.text.to_vec();
                let count = self.take_count();
                let range = if key == Key::Char('S') {
                    motion::line_object(&data, self.cursor, false)
                } else {
                    let mut end = self.cursor;
                    for _ in 0..count {
                        end = motion::char_next(&data, end);
                    }
                    self.cursor..end
                };
                return self.apply_operator(range, key == Key::Char('S'));
            }
            Key::Char('p' | 'P') => return self.put(key == Key::Char('p')),
            Key::Char('r') => {
                if self.queued_keys.is_empty() {
                    self.language_map_disabled = true;
                }
                self.pending = Pending::ReplaceChar;
            }
            // The native visual table aliases `u`/`U` to the selection case
            // operators followed by Escape; they never reach history here.
            Key::Char('u') if self.is_visual() => {
                let action = self.selection_case(CaseFold::Lower);
                self.window_selection_save()?;
                self.cancel();
                return action;
            }
            Key::Char('U') if self.is_visual() => {
                let action = self.selection_case(CaseFold::Upper);
                self.window_selection_save()?;
                self.cancel();
                return action;
            }
            Key::Char('u') => {
                let count = self.take_count();
                self.history_move(false, count)?;
            }
            Key::Ctrl('r') => {
                let count = self.take_count();
                self.history_move(true, count)?;
            }
            Key::Ctrl('z') => {
                return self.invoke_native_action(NativeAction::EditorSuspend);
            }
            Key::Char('v' | 'V') => {
                let mode = if key == Key::Char('v') {
                    Mode::Visual
                } else {
                    Mode::VisualLine
                };
                if self.mode == mode {
                    self.mode = Mode::Normal;
                } else {
                    // Native visual-line entry runs a no-op motion that
                    // consumes the pending count before the next key.
                    if mode == Mode::VisualLine {
                        self.count = None;
                    }
                    if !self.is_visual() {
                        self.anchor = self.cursor;
                    }
                    self.mode = mode;
                    self.materialize_visual_lines()?;
                }
            }
            Key::Char('I' | 'A') if self.is_visual() => {
                return self.line_cursors(key == Key::Char('A'));
            }
            Key::Tab if !self.is_visual() => {
                self.align_cursors()?;
            }
            Key::Tab | Key::BackTab => {
                self.align_indentation(key == Key::BackTab)?;
            }
            Key::Char('o') if self.is_visual() => {
                self.selection_set.flip_all();
                self.sync_set_to_primary();
            }
            Key::Char('J') => return self.join(),
            Key::Char(':') => return self.show_prompt(Prompt::Command),
            Key::Char('/' | '?') => {
                return self.show_prompt(Prompt::Search {
                    backwards: key == Key::Char('?'),
                });
            }
            Key::Char('n' | 'N') => {
                let count = self.take_count();
                for _ in 0..count {
                    self.search_motion(self.search_backwards ^ (key == Key::Char('N')))?;
                }
                self.jumplist_save();
            }
            Key::Char('*' | '#') => {
                let data = self.text.to_vec();
                let range = motion::word_object(&data, self.cursor, false, false);
                if range.start > range.end || range.end > data.len() {
                    return Err(EditorError("no word at cursor".into()));
                }
                let word = &data[range];
                self.word_search = Some(word.to_vec());
                self.search = Self::escape_pattern(word);
                self.search_backwards = key == Key::Char('#');
                self.session
                    .set_previous_regex(&self.search)
                    .map_err(|e| EditorError(e.to_string()))?;
                self.registers.insert(
                    '/',
                    Register {
                        bytes: self.search.as_bytes().to_vec(),
                        linewise: false,
                    },
                );
                let next = if key == Key::Char('*') {
                    motion::word_find_next(&data, motion::char_next(&data, self.cursor), word)
                } else {
                    motion::word_find_prev(&data, self.cursor, word)
                };
                if let Some(range) = next {
                    self.cursor = range.start;
                    self.request_centered_follow();
                }
                self.jumplist_save();
            }
            Key::Char('z' | 'Z') => self.pending = Pending::Z,
            Key::PageDown => return self.invoke_native_action(NativeAction::WindowPageDown),
            Key::PageUp => return self.invoke_native_action(NativeAction::WindowPageUp),
            Key::ShiftPageDown => {
                return self.invoke_native_action(NativeAction::WindowHalfpageDown);
            }
            Key::ShiftPageUp => return self.invoke_native_action(NativeAction::WindowHalfpageUp),
            Key::Ctrl('e') => return self.invoke_native_action(NativeAction::WindowSlideUp),
            Key::Ctrl('y') => return self.invoke_native_action(NativeAction::WindowSlideDown),
            Key::Char('q') => self.pending = Pending::MacroRecord,
            Key::Char('@') => self.pending = Pending::MacroPlay,
            Key::Ctrl('w') => self.pending = Pending::Window,
            Key::Function(1) => return Ok(EditorAction::HostCommand("help".into())),
            Key::Ctrl('j' | 'k') | Key::AltCtrl('j' | 'k') if !self.is_visual() => {
                let direction = match key {
                    Key::AltCtrl('k') => i64::from(i32::MIN),
                    Key::AltCtrl('j') => i64::from(i32::MAX),
                    Key::Ctrl('k') => -1,
                    _ => 1,
                };
                self.native_new_selection(direction)?;
            }
            Key::Ctrl('d' | 'u' | 'j' | 'k') => {
                // The native navigate action scrolls half a page while a
                // single selection is active and only rotates the primary
                // selection among several.
                let down = matches!(key, Key::Ctrl('d' | 'j'));
                if self.selection_set.len() == 1 {
                    return self.invoke_native_action(if down {
                        NativeAction::WindowHalfpageDown
                    } else {
                        NativeAction::WindowHalfpageUp
                    });
                }
                let count = self.take_count() as isize;
                self.selection_set
                    .navigate(if down { count } else { -count });
                self.sync_set_to_primary();
            }
            Key::Ctrl('p') => {
                self.selection_set.remove_primary();
                self.sync_set_to_primary();
            }
            Key::Ctrl('n') if !self.is_visual() => {
                self.mode = Mode::Visual;
                return self.text_object('w', false);
            }
            Key::Ctrl('n' | 'a' | 'x') => {
                self.select_matching(key == Key::Ctrl('a'), key == Key::Ctrl('x'))?;
            }
            Key::Ctrl('c') => {
                let data = self.text.to_vec();
                let column = self
                    .count
                    .take()
                    .map_or(usize::MAX, |n| n.saturating_sub(1));
                if self.selection_set.len() == 1 {
                    self.cancel();
                } else {
                    self.selection_set
                        .remove_column(&data, column)
                        .map_err(|e| EditorError(e.to_string()))?;
                    self.sync_set_to_primary();
                }
            }
            Key::Ctrl('l') => {
                let data = self.text.to_vec();
                let column = self
                    .count
                    .take()
                    .map_or(usize::MAX, |n| n.saturating_sub(1));
                self.selection_set
                    .keep_column(&data, column)
                    .map_err(|e| EditorError(e.to_string()))?;
                self.sync_set_to_primary();
            }
            _ => {
                // The native editor silently discards a key that no binding
                // in the active mode chain matches; the visual tables bind
                // fewer keys than the normal table, so D/C/Y and friends end
                // here as no-ops instead of errors.
                if self.is_visual() {
                    return Ok(EditorAction::Continue);
                }
                return Err(EditorError("unsupported key in this mode".into()));
            }
        }
        Ok(EditorAction::Continue)
    }

    fn is_visual(&self) -> bool {
        matches!(self.mode, Mode::Visual | Mode::VisualLine)
    }

    fn combined_count(&mut self) -> Result<usize, EditorError> {
        self.take_count()
            .checked_mul(self.operator_count)
            .filter(|n| *n <= MAX_COUNT)
            .ok_or_else(|| EditorError("count limit exceeded".into()))
    }

    fn motion(&mut self, key: Key) -> Result<Option<(usize, bool, bool, bool)>, EditorError> {
        self.motion_targets.clear();
        if self.selection_set.len() == 1 {
            return self.motion_one(key);
        }
        let original_cursor = self.cursor;
        let original_count = self.count;
        let original_column = self.preferred_column;
        let selections = self.selection_set.selections().to_vec();
        let primary = self.primary_index();
        let mut result = None;
        let mut preferred = None;
        for (index, selection) in selections.into_iter().enumerate() {
            self.cursor = selection.cursor;
            self.count = original_count;
            self.preferred_column = original_column;
            let target = self.motion_one(key)?;
            let Some((cursor, inclusive, linewise, linewise_inclusive)) = target else {
                self.cursor = original_cursor;
                self.count = original_count;
                self.preferred_column = original_column;
                self.motion_targets.clear();
                return Ok(None);
            };
            self.motion_targets.push(Selection {
                anchor: if self.is_visual() {
                    selection.anchor
                } else {
                    cursor
                },
                cursor,
            });
            if index == primary {
                result = Some((cursor, inclusive, linewise, linewise_inclusive));
                preferred = self.preferred_column;
            }
        }
        self.cursor = original_cursor;
        self.count = None;
        self.preferred_column = preferred;
        Ok(result)
    }

    fn motion_one(&mut self, key: Key) -> Result<Option<(usize, bool, bool, bool)>, EditorError> {
        let data = self.text.to_vec();
        let mut pos = self.cursor;
        let count = self.count.unwrap_or(1);
        let count = if self.operator.is_some() {
            count
                .checked_mul(self.operator_count)
                .filter(|n| *n <= MAX_COUNT)
                .ok_or_else(|| EditorError("count limit exceeded".into()))?
        } else {
            count
        };
        let mut inclusive = false;
        let mut linewise = false;
        let mut linewise_inclusive = false;
        if matches!(key, Key::Char('+' | '-')) {
            if self.is_visual() {
                return Ok(None);
            }
            for _ in 0..count {
                let next = if key == Key::Char('+') {
                    motion::line_next(&data, pos)
                } else {
                    motion::line_begin(&data, motion::line_prev(&data, pos))
                };
                if next >= data.len() {
                    break;
                }
                pos = next;
            }
            self.count = None;
            return Ok(Some((motion::line_start(&data, pos), false, true, false)));
        }
        if key == Key::Char('%') && self.count.is_some() {
            self.count = None;
            return Ok(Some((
                data.len() / 100 * count.min(100) + data.len() % 100 * count.min(100) / 100,
                false,
                false,
                false,
            )));
        }
        if matches!(key, Key::Char('j' | 'k') | Key::Up | Key::Down | Key::Enter) {
            let column = self
                .preferred_column
                .unwrap_or_else(|| self.display_column(&data, pos));
            self.preferred_column = Some(column);
            for _ in 0..count {
                let next = if matches!(key, Key::Char('k') | Key::Up) {
                    if motion::line_begin(&data, pos) == 0 {
                        break;
                    }
                    motion::line_begin(&data, motion::line_prev(&data, pos))
                } else {
                    let next = motion::line_next(&data, pos);
                    if next >= data.len() {
                        break;
                    }
                    next
                };
                pos = self.column_position(&data, next, column);
            }
            self.count = None;
            return Ok(Some((pos, false, true, true)));
        }
        if key == Key::Char('G') {
            // G without a count is native FILE_END (LINEWISE_INCLUSIVE);
            // a counted G is the non-inclusive line motion.
            pos = if let Some(line) = self.count {
                Self::line_number(&data, line)
            } else {
                linewise_inclusive = true;
                motion::line_begin(&data, data.len().saturating_sub(1))
            };
            linewise = true;
        } else {
            let func: fn(&[u8], usize) -> usize = match key {
                Key::Char('h') | Key::Left | Key::Backspace => motion::char_prev,
                Key::Char('l') | Key::Right => motion::char_next,
                Key::Char('j') | Key::Down | Key::Enter => {
                    linewise = true;
                    motion::line_down
                }
                Key::Char('k') | Key::Up => {
                    linewise = true;
                    motion::line_up
                }
                Key::Char('w') => motion::word_start_next,
                Key::Char('W') | Key::ShiftRight => motion::longword_start_next,
                Key::Char('b') => motion::word_start_prev,
                Key::Char('B') | Key::ShiftLeft => motion::longword_start_prev,
                Key::Char('e') => {
                    inclusive = true;
                    motion::word_end_next
                }
                Key::Char('E') => {
                    inclusive = true;
                    motion::longword_end_next
                }
                Key::Char('0') | Key::Home => motion::line_begin,
                Key::Char('^') => motion::line_start,
                Key::Char('$') | Key::End => motion::line_end,
                // Native paragraph motions are LINEWISE|JUMP.
                Key::Char('{') => {
                    linewise = true;
                    motion::paragraph_prev
                }
                Key::Char('}') => {
                    linewise = true;
                    motion::paragraph_next
                }
                Key::Char('(') => motion::sentence_prev,
                Key::Char(')') => motion::sentence_next,
                Key::Char('%') => {
                    inclusive = true;
                    motion::bracket_match_or_next
                }
                _ => return Ok(None),
            };
            // C maps cw/cW to the inclusive word end while preserving whitespace.
            if matches!(self.operator, Some(Operator::Change))
                && matches!(key, Key::Char('w' | 'W'))
                && data.get(pos).is_some_and(|b| !b.is_ascii_whitespace())
            {
                let end = if key == Key::Char('w') {
                    motion::word_end_next
                } else {
                    motion::longword_end_next
                };
                let boundary = |b: u8| {
                    if key == Key::Char('W') {
                        b.is_ascii_whitespace()
                    } else {
                        b.is_ascii() && !(b.is_ascii_alphanumeric() || b == b'_')
                    }
                };
                let next = motion::char_next(&data, pos);
                if !boundary(data[pos]) && data.get(next).is_none_or(|b| boundary(*b)) {
                    if count == 1 {
                        pos = next;
                    } else {
                        for _ in 1..count {
                            pos = end(&data, pos);
                        }
                        inclusive = true;
                    }
                } else {
                    for _ in 0..count {
                        pos = end(&data, pos);
                    }
                    inclusive = true;
                }
            } else {
                for _ in 0..count {
                    pos = func(&data, pos);
                }
            }
        }
        self.count = None;
        Ok(Some((
            pos.min(data.len()),
            inclusive,
            linewise,
            linewise_inclusive,
        )))
    }

    fn line_number(data: &[u8], line: usize) -> usize {
        let mut pos = 0;
        for _ in 1..line {
            let next = motion::line_next(data, pos);
            if next >= data.len() {
                break;
            }
            pos = next;
        }
        pos
    }

    fn display_column(&self, data: &[u8], pos: usize) -> usize {
        if pos == data.len() && data.last() == Some(&b'\n') {
            return 1;
        }
        let mut p = motion::line_begin(data, pos);
        let mut column = 0;
        while p < pos.min(data.len()) {
            column += if data[p] == b'\t' {
                self.tab_width.max(1) - column % self.tab_width.max(1)
            } else {
                motion::codepoint_width(data, p)
            };
            p = motion::codepoint_next(data, p);
        }
        column
    }

    fn column_position(&self, data: &[u8], start: usize, desired: usize) -> usize {
        let mut p = start;
        let mut column = 0;
        while p < data.len() && data[p] != b'\n' {
            let width = if data[p] == b'\t' {
                self.tab_width.max(1) - column % self.tab_width.max(1)
            } else {
                motion::codepoint_width(data, p)
            };
            if column + width > desired {
                break;
            }
            column += width;
            p = motion::codepoint_next(data, p);
        }
        p
    }

    fn finish_motion(
        &mut self,
        target: usize,
        inclusive: bool,
        linewise: bool,
    ) -> Result<EditorAction, EditorError> {
        self.finish_motion_with(target, inclusive, linewise, None)
    }

    /// Finish a motion whose native type is known, including whether a
    /// linewise motion is `LINEWISE_INCLUSIVE` (`j`/`k`, `G` without a count).
    fn finish_motion_native(
        &mut self,
        target: usize,
        inclusive: bool,
        linewise: bool,
        linewise_inclusive: bool,
    ) -> Result<EditorAction, EditorError> {
        self.finish_motion_with(target, inclusive, linewise, Some(linewise_inclusive))
    }

    fn finish_motion_with(
        &mut self,
        target: usize,
        inclusive: bool,
        linewise: bool,
        linewise_inclusive: Option<bool>,
    ) -> Result<EditorAction, EditorError> {
        if !self.motion_targets.is_empty() {
            self.external_marks.clear();
            let targets = std::mem::take(&mut self.motion_targets);
            let data = self.text.to_vec();
            if self.operator.is_some() {
                let ranges = self
                    .selection_set
                    .selections()
                    .iter()
                    .zip(targets)
                    .map(|(source, target)| {
                        let start = source.cursor.min(target.cursor);
                        let end = source.cursor.max(target.cursor);
                        if linewise {
                            self.linewise_operator_range(&data, start, end, linewise_inclusive)
                        } else {
                            start..if inclusive && target.cursor > source.cursor {
                                motion::char_next(&data, end)
                            } else {
                                end
                            }
                        }
                    })
                    .collect();
                return self.apply_many_operators(ranges, linewise);
            }
            self.selection_set
                .replace_selections(&data, targets, self.primary_index())
                .map_err(|e| EditorError(e.to_string()))?;
            self.sync_set_to_primary();
            self.materialize_visual_lines()?;
            return Ok(EditorAction::Continue);
        }
        if self.operator.is_some() {
            let data = self.text.to_vec();
            let start = self.cursor.min(target);
            let end = self.cursor.max(target);
            let range = if linewise {
                self.linewise_operator_range(&data, start, end, linewise_inclusive)
            } else {
                start..if inclusive && target > self.cursor {
                    motion::char_next(&data, end)
                } else {
                    end
                }
            };
            self.apply_operator(range, linewise)
        } else {
            self.external_marks.clear();
            let data = self.text.to_vec();
            let cursor = target.min(data.len());
            self.selection_set
                .replace_primary(
                    &data,
                    Selection {
                        anchor: if self.is_visual() {
                            self.anchor
                        } else {
                            cursor
                        },
                        cursor,
                    },
                )
                .map_err(|e| EditorError(e.to_string()))?;
            self.sync_set_to_primary();
            self.materialize_visual_lines()?;
            Ok(EditorAction::Continue)
        }
    }

    /// Native operator range for a linewise motion. `text_range_linewise`
    /// expands the start to the line begin and the end to the next line only
    /// when it is not already at a line begin; `LINEWISE_INCLUSIVE` motions
    /// first extend the end by one character.
    fn linewise_operator_range(
        &self,
        data: &[u8],
        start: usize,
        end: usize,
        linewise_inclusive: Option<bool>,
    ) -> Range<usize> {
        let start = motion::line_begin(data, start);
        match linewise_inclusive {
            None => start..motion::line_next(data, end),
            Some(inclusive) => {
                let end = if inclusive {
                    motion::char_next(data, end)
                } else {
                    end
                };
                let end = if end == motion::line_begin(data, end) {
                    end
                } else {
                    motion::line_next(data, end)
                };
                start..end
            }
        }
    }

    fn g_key(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        match key {
            Key::Char('s') => {
                self.count = None;
                self.jumplist_save();
                return Ok(EditorAction::Continue);
            }
            Key::Char('<') => return self.jumplist_navigate(-1),
            Key::Char('>') => return self.jumplist_navigate(1),
            Key::Char('-') => return self.invoke_native_action(NativeAction::Earlier),
            Key::Char('+') => return self.invoke_native_action(NativeAction::Later),
            Key::Char('|') => return self.invoke_native_action(NativeAction::CursorColumn),
            Key::Char('o') => return self.invoke_native_action(NativeAction::CursorByte),
            Key::Char('H') => return self.invoke_native_action(NativeAction::CursorByteLeft),
            Key::Char('L') => return self.invoke_native_action(NativeAction::CursorByteRight),
            Key::Char('U') => return self.selection_case(CaseFold::Upper),
            Key::Char('u') => return self.selection_case(CaseFold::Lower),
            Key::Char('~') => return self.selection_case(CaseFold::Toggle),
            _ => {}
        }
        match key {
            Key::Char('j') => return self.invoke_native_action(NativeAction::CursorScreenLineDown),
            Key::Char('k') => return self.invoke_native_action(NativeAction::CursorScreenLineUp),
            Key::Char('m') => {
                return self.invoke_native_action(NativeAction::CursorScreenLineMiddle);
            }
            Key::Char('$') => return self.invoke_native_action(NativeAction::CursorScreenLineEnd),
            _ => {}
        }
        if key == Key::Char('J') {
            return self.join_with_space(false);
        }
        if matches!(key, Key::Char('n' | 'N')) {
            return self.search_object(key == Key::Char('N'));
        }
        if key == Key::Char('v') {
            self.restore_selection_mark('^', true)?;
            return Ok(EditorAction::Continue);
        }
        if matches!(key, Key::Char('a' | '8')) {
            let data = self.text.to_vec();
            let end = motion::codepoint_next(&data, self.cursor);
            let bytes = &data[self.cursor..end];
            return Ok(EditorAction::Message(if key == Key::Char('8') {
                bytes
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                String::from_utf8_lossy(bytes)
                    .chars()
                    .next()
                    .map_or_else(|| "EOF".into(), |c| format!("U+{:04X}", c as u32))
            }));
        }
        let data = self.text.to_vec();
        let count = self.take_count();
        if self.selection_set.len() > 1 {
            self.motion_targets.clear();
            let mut result = None;
            for (index, selection) in self.selection_set.selections().iter().enumerate() {
                let (cursor, inclusive, linewise) =
                    Self::g_target(&data, selection.cursor, key, count)?;
                self.motion_targets.push(Selection {
                    anchor: if self.is_visual() {
                        selection.anchor
                    } else {
                        cursor
                    },
                    cursor,
                });
                if index == self.primary_index() {
                    result = Some((cursor, inclusive, linewise));
                }
            }
            let (target, inclusive, linewise) = result.expect("selection set has a primary");
            let action = self.finish_motion(target, inclusive, linewise)?;
            if key == Key::Char('g') {
                self.jumplist_save();
            }
            return Ok(action);
        }
        let (target, inclusive, linewise) = Self::g_target(&data, self.cursor, key, count)?;
        let action = self.finish_motion(target, inclusive, linewise)?;
        if key == Key::Char('g') {
            self.jumplist_save();
        }
        Ok(action)
    }

    /// Motion keys whose native movement carries the JUMP flag.
    fn jump_motion_key(key: Key) -> bool {
        matches!(key, Key::Char('G' | '{' | '}' | '(' | ')' | '%'))
    }

    fn g_target(
        data: &[u8],
        cursor: usize,
        key: Key,
        count: usize,
    ) -> Result<(usize, bool, bool), EditorError> {
        let (target, inclusive, linewise) = match key {
            Key::Char('g') => (Self::line_number(data, count), false, true),
            Key::Char('e') => {
                let mut p = cursor;
                for _ in 0..count {
                    p = motion::word_end_prev(data, p);
                }
                (p, true, false)
            }
            Key::Char('E') => {
                let mut p = cursor;
                for _ in 0..count {
                    p = motion::longword_end_prev(data, p);
                }
                (p, true, false)
            }
            Key::Char('h') => (motion::codepoint_prev(data, cursor), false, false),
            Key::Char('l') => (motion::codepoint_next(data, cursor), false, false),
            Key::Char('_') => (motion::line_finish(data, cursor), true, false),
            Key::Char('0') => (motion::line_begin(data, cursor), false, false),
            _ => return Err(EditorError("unsupported g command".into())),
        };
        Ok((target, inclusive, linewise))
    }

    fn text_object(&mut self, c: char, outer: bool) -> Result<EditorAction, EditorError> {
        let data = self.text.to_vec();
        let count = self.combined_count()?;
        let ranges = self
            .selection_set
            .selections()
            .iter()
            .map(|selection| Self::object_range(&data, selection.cursor, c, outer, count))
            .collect::<Result<Vec<_>, _>>()?;
        if self.operator.is_some() {
            if ranges.len() > 1 {
                self.apply_many_operators(ranges, false)
            } else {
                self.apply_operator(ranges[0].clone(), false)
            }
        } else {
            let ranges = if self.is_visual() {
                ranges
                    .into_iter()
                    .zip(self.selection_ranges())
                    .map(|(object, source)| {
                        source.start.min(object.start)..source.end.max(object.end)
                    })
                    .collect()
            } else {
                self.mode = Mode::Visual;
                ranges
            };
            self.adopt_ranges(ranges)?;
            self.materialize_visual_lines()?;
            Ok(EditorAction::Continue)
        }
    }

    fn object_range(
        data: &[u8],
        cursor: usize,
        c: char,
        outer: bool,
        count: usize,
    ) -> Result<Range<usize>, EditorError> {
        let mut range = cursor..cursor;
        let mut pos = cursor;
        for iteration in 0..count {
            let object = match c {
                'w' | 'W' => Some(motion::word_object(data, pos, outer, c == 'W')),
                'p' => Some(motion::paragraph_object(data, pos, outer)),
                's' => Some(motion::sentence_object(data, pos)),
                'l' => Some(motion::line_object(data, pos, !outer)),
                '\t' => Some(motion::indentation_object(data, pos)),
                '(' | ')' | 'b' => motion::delimited_object(data, pos, b'(', outer),
                '{' | '}' | 'B' => motion::delimited_object(data, pos, b'{', outer),
                '[' | ']' => motion::delimited_object(data, pos, b'[', outer),
                '<' | '>' => motion::delimited_object(data, pos, b'<', outer),
                '"' | '\'' | '`' => motion::delimited_object(data, pos, c as u8, outer),
                _ => return Err(EditorError("unknown text object".into())),
            };
            // Native vis_do breaks out of the count loop when the object is
            // missing, leaving the range accumulated so far (an empty range
            // at the cursor for the first iteration) for the operator.
            let Some(object) = object else {
                break;
            };
            if object.start > object.end || object.end > data.len() {
                return Err(EditorError("invalid text object range".into()));
            }
            if iteration == 0 {
                range = object.clone();
            } else {
                range.start = range.start.min(object.start);
                range.end = range.end.max(object.end);
            }
            if object.end >= data.len() {
                break;
            }
            pos = if !outer
                && matches!(
                    c,
                    '(' | ')' | 'b' | '{' | '}' | 'B' | '[' | ']' | '<' | '>' | '"' | '\'' | '`'
                ) {
                object.end.saturating_add(1).min(data.len())
            } else {
                object.end
            };
        }
        Ok(range)
    }

    fn store_register(&mut self, bytes: Vec<u8>, linewise: bool, yank: bool) {
        let name = self.operation_register_name();
        if name == '_' {
            // The black-hole register discards bytes but retains the operator's
            // linewise flag, which affects a subsequent empty put's cursor.
            self.registers.insert(
                '_',
                Register {
                    bytes: Vec::new(),
                    linewise,
                },
            );
            return;
        }
        let append = name.is_ascii_uppercase();
        let name = name.to_ascii_lowercase();
        let register = if append {
            let mut register = self.registers.get(&name).cloned().unwrap_or_default();
            register.bytes.extend_from_slice(&bytes);
            register.linewise = linewise;
            register
        } else {
            Register { bytes, linewise }
        };
        self.registers.insert(name, register.clone());
        if !self.internal_scope && yank && name == '"' {
            self.registers.insert('0', register);
        }
    }

    /// Insert the newline used by native `op_change` for a linewise range.
    fn insert_change_newline(&mut self, mut pos: usize) -> Result<usize, EditorError> {
        let eof = pos == self.text.len();
        let second_newline = eof && (pos == 0 || self.text.byte(pos - 1) != Some(b'\n'));
        let mut indent = Vec::new();
        let mut indent_len = 0;
        if self.auto_indent {
            let data = self.text.to_vec();
            let begin = motion::line_begin(&data, pos);
            let mut start = motion::line_start(&data, begin);
            let end = motion::line_end(&data, start);
            if start > pos {
                start = pos;
            }
            indent_len = start.saturating_sub(begin);
            if start == end {
                pos = begin;
            } else {
                indent.extend_from_slice(&data[begin..begin + indent_len]);
            }
        }
        self.text.insert(pos, b"\n")?;
        if eof {
            if second_newline {
                self.text.insert(self.text.len(), b"\n")?;
            } else {
                pos = pos.saturating_sub(1);
            }
        }
        pos += 1;
        if !indent.is_empty() {
            self.text.insert(pos, &indent)?;
        }
        Ok(pos + indent_len)
    }

    fn apply_operator(
        &mut self,
        range: Range<usize>,
        linewise: bool,
    ) -> Result<EditorAction, EditorError> {
        if self.is_visual() {
            self.window_selection_save()?;
        }
        if self.selection_set.len() > 1 && self.is_visual() {
            return self.apply_many_operators(self.selection_ranges(), linewise);
        }
        if range.start > range.end || range.end > self.text.len() {
            return Err(EditorError("invalid operator range".into()));
        }
        if let Some(Operator::Callback(id)) = self.operator {
            return self.start_callback_batch(id, CallbackType::Operator, 1, Some(vec![range]));
        }
        let operator = self
            .operator
            .take()
            .ok_or_else(|| EditorError("operator missing".into()))?;
        self.operator_count = 1;
        self.count = None;
        let was_visual = self.is_visual();
        let change_linewise =
            matches!(operator, Operator::Change) && native_linewise_range(&self.text, &range);
        self.mode = Mode::Normal;
        match operator {
            Operator::Yank => {
                self.recording_change = true;
                self.store_register(
                    self.text.bytes(range.clone()).unwrap_or_default(),
                    linewise,
                    true,
                );
                // Native `op_yank` returns the cursor position for linewise
                // ranges and the range start otherwise, so a linewise yank
                // never moves the cursor.
                if !linewise {
                    self.cursor = range.start;
                }
            }
            Operator::Delete | Operator::Change => {
                self.text.snapshot();
                self.store_register(
                    self.text.bytes(range.clone()).unwrap_or_default(),
                    linewise,
                    false,
                );
                self.text.delete(range.clone())?;
                self.cursor = range.start.min(self.text.len());
                self.recording_change = true;
                if matches!(operator, Operator::Change) {
                    if linewise || change_linewise {
                        let original_position = self.cursor;
                        let newline_position =
                            original_position.saturating_sub(usize::from(original_position > 0));
                        let new_cursor = self.insert_change_newline(newline_position)?;
                        if original_position > 0 {
                            self.cursor = new_cursor;
                        }
                        self.recording_change = true;
                    }
                    self.enter_insert(false, 1);
                } else {
                    if linewise && self.cursor == self.text.len() {
                        let data = self.text.to_vec();
                        self.cursor =
                            motion::line_begin(&data, motion::line_prev(&data, self.cursor));
                    }
                    self.text.snapshot();
                }
            }
            Operator::Callback(_) => {
                return Err(EditorError(
                    "callback operator requires host dispatch".into(),
                ));
            }
            Operator::ShiftLeft | Operator::ShiftRight => {
                self.shift(range, matches!(operator, Operator::ShiftRight))?
            }
        }
        if was_visual {
            // Native `vis_mode_visual_leave` keeps only the cursor endpoints;
            // the operator already resolved the final primary cursor.
            self.set_cursors(vec![self.cursor], self.primary_index())?;
        }
        Ok(EditorAction::Continue)
    }

    fn store_many_registers(&mut self, ranges: &[Range<usize>], linewise: bool, yank: bool) {
        let name = self.operation_register_name();
        let key = name.to_ascii_lowercase();
        let previous = self.register_slots.get(&key).cloned().unwrap_or_default();
        let mut slots: Vec<_> = ranges
            .iter()
            .map(|range| Register {
                bytes: self.text.bytes(range.clone()).unwrap_or_default(),
                linewise,
            })
            .collect();
        if let Some(first) = slots.first() {
            self.store_register(first.bytes.clone(), linewise, yank);
        }
        if name == '_' {
            self.register_slots.insert(
                '_',
                slots
                    .into_iter()
                    .map(|_| Register {
                        bytes: Vec::new(),
                        linewise,
                    })
                    .collect(),
            );
            return;
        }
        if name.is_ascii_uppercase() {
            for (index, slot) in slots.iter_mut().enumerate() {
                if let Some(old) = previous.get(index) {
                    let mut bytes = old.bytes.clone();
                    bytes.extend_from_slice(&slot.bytes);
                    slot.bytes = bytes;
                }
            }
        }
        self.register_slots.insert(key, slots.clone());
        if !self.internal_scope && yank && name == '"' {
            self.register_slots.insert('0', slots);
        }
    }

    fn apply_many_operators(
        &mut self,
        ranges: Vec<Range<usize>>,
        linewise: bool,
    ) -> Result<EditorAction, EditorError> {
        if let Some(Operator::Callback(id)) = self.operator {
            return self.start_callback_batch(id, CallbackType::Operator, 1, Some(ranges));
        }
        let operator = self
            .operator
            .take()
            .ok_or_else(|| EditorError("operator missing".into()))?;
        self.operator_count = 1;
        self.count = None;
        for range in &ranges {
            if range.start > range.end || range.end > self.text.len() {
                return Err(EditorError(
                    "invalid multiple-selection operator range".into(),
                ));
            }
        }
        if matches!(operator, Operator::ShiftLeft | Operator::ShiftRight) {
            if self.is_visual() {
                self.window_selection_save()?;
            }
            self.shift_ranges(ranges, matches!(operator, Operator::ShiftRight))?;
            return Ok(EditorAction::Continue);
        }
        self.mode = Mode::Normal;
        self.store_many_registers(&ranges, linewise, matches!(operator, Operator::Yank));
        match operator {
            Operator::Callback(_) => {
                return Err(EditorError(
                    "callback operator must dispatch before native transaction".into(),
                ));
            }
            Operator::Yank => {
                let positions = ranges
                    .iter()
                    .enumerate()
                    .map(|(index, range)| {
                        // Native `op_yank` keeps every cursor in place for a
                        // linewise yank and moves to the range start otherwise.
                        if linewise {
                            self.selection_set.selections()[index].cursor
                        } else {
                            range.start
                        }
                    })
                    .collect();
                self.set_cursors(positions, self.primary_index())?;
            }
            Operator::Delete | Operator::Change => {
                self.text.snapshot();
                let change = matches!(operator, Operator::Change);
                let edits = ranges
                    .into_iter()
                    .map(|range| {
                        let change_linewise =
                            change && (linewise || native_linewise_range(&self.text, &range));
                        (
                            range,
                            if change_linewise {
                                vec![b'\n']
                            } else {
                                Vec::new()
                            },
                        )
                    })
                    .collect();
                self.commit_multiple(edits, Placement::Start)?;
                if change {
                    self.enter_insert(false, 1);
                } else {
                    self.text.snapshot();
                }
            }
            _ => unreachable!("shifts handled above"),
        }
        Ok(EditorAction::Continue)
    }

    fn run_line_operator(
        &mut self,
        ranges: Option<Vec<Range<usize>>>,
        mut operation: impl FnMut(
            &mut LineOperatorPlan,
            Option<Range<usize>>,
            usize,
        ) -> Result<usize, EditorError>,
    ) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        if ranges.as_ref().is_some_and(|r| {
            r.len() != self.selections().len()
                || r.iter().any(|r| r.start > r.end || r.end > data.len())
        }) {
            return Err(EditorError("invalid native line-operator ranges".into()));
        }
        let mut plan = LineOperatorPlan::new(&data);
        let mut sources: Vec<_> = self
            .view_identified()
            .into_iter()
            .map(|(id, s)| (id, plan.text.mark(s.cursor)))
            .collect();
        let range_marks = ranges.as_ref().map(|ranges| {
            ranges
                .iter()
                .map(|r| {
                    (
                        plan.text.mark(r.start).expect("validated start"),
                        plan.text.mark(r.end).expect("validated end"),
                    )
                })
                .collect::<Vec<_>>()
        });
        for index in 0..sources.len() {
            let Some(cursor) = sources[index]
                .1
                .as_ref()
                .and_then(|m| plan.text.mark_get(m.clone()))
            else {
                sources[index].1 = None;
                continue;
            };
            let range = range_marks.as_ref().map(|ranges| {
                let (start, end) = &ranges[index];
                let start = plan.text.mark_get(start.clone()).unwrap_or(cursor);
                let end = plan.text.mark_get(end.clone()).unwrap_or(cursor);
                if start <= end {
                    start..end
                } else {
                    cursor..cursor
                }
            });
            let cursor = operation(&mut plan, range, cursor)?.min(plan.text.len());
            sources[index].1 = plan.text.mark(cursor);
        }
        let final_data = plan.text.to_vec();
        let mut selections: Vec<_> = sources
            .iter()
            .filter_map(|(id, mark)| {
                let cursor = plan.text.mark_get(mark.as_ref()?.clone())?;
                Some((
                    *id,
                    Selection {
                        anchor: cursor,
                        cursor,
                    },
                ))
            })
            .collect();
        let old_primary = self.selection_set.primary_id();
        let primary = if selections.iter().any(|(id, _)| *id == old_primary) {
            old_primary
        } else {
            let old_index = self.primary_index();
            sources
                .iter()
                .skip(old_index)
                .find_map(|(id, _)| selections.iter().any(|(live, _)| live == id).then_some(*id))
                .or_else(|| selections.last().map(|(id, _)| *id))
                .unwrap_or_else(|| sources.last().expect("selection set").0)
        };
        if selections.is_empty() {
            selections.push((
                primary,
                Selection {
                    anchor: 0,
                    cursor: 0,
                },
            ));
        }
        let mut set = SelectionSet::from_identified(&final_data, selections, primary)
            .map_err(|e| EditorError(e.to_string()))?;
        set.normalize_native(&final_data)
            .map_err(|e| EditorError(e.to_string()))?;
        // All edits and final placements have been checked on the scratch text.
        // Commit the same sequential transcript so original byte marks survive.
        self.text.snapshot();
        for (range, bytes) in plan.edits {
            self.text.replace(range, &bytes)?;
        }
        self.text.snapshot();
        self.selection_set = set;
        self.external_marks.clear();
        self.mode = Mode::Normal;
        self.operator = None;
        self.operator_count = 1;
        self.count = None;
        for id in self.selection_ids().to_vec() {
            self.anchored.insert(id, false);
        }
        self.sync_set_to_primary();
        self.recording_change = true;
        Ok(())
    }
    fn shift_ranges(&mut self, ranges: Vec<Range<usize>>, right: bool) -> Result<(), EditorError> {
        let expand = self.expand_tab;
        let width = self.tab_width.clamp(1, 8);
        self.run_line_operator(Some(ranges), |plan, range, cursor| {
            plan.shift(range.expect("shift ranges"), cursor, right, expand, width)
        })
    }
    fn shift(&mut self, range: Range<usize>, right: bool) -> Result<(), EditorError> {
        self.shift_ranges(vec![range], right)
    }

    fn enter_insert(&mut self, replace: bool, count: usize) {
        self.recording_change = true;
        self.mode = if replace { Mode::Replace } else { Mode::Insert };
        self.insert_count = count;
        self.insert_keys.clear();
    }

    fn insert_key(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        let previous_newline = std::mem::take(&mut self.last_insert_newline);
        match self.pending {
            Pending::InsertDigraph => {
                if let Key::Char(c) = key {
                    let mut bytes = [0; 4];
                    self.digraph_bytes
                        .extend_from_slice(c.encode_utf8(&mut bytes).as_bytes());
                    match crate::digraph::lookup(&self.digraph_bytes) {
                        crate::digraph::Lookup::Found(c) => {
                            self.pending = Pending::None;
                            let mut bytes = [0; 4];
                            self.insert_literal_bytes(c.encode_utf8(&mut bytes).as_bytes())?;
                        }
                        crate::digraph::Lookup::Prefix => {}
                        crate::digraph::Lookup::Unknown => {
                            self.pending = Pending::None;
                            return Ok(EditorAction::Message("unknown digraph".into()));
                        }
                    }
                    return Ok(EditorAction::Continue);
                }
                self.pending = Pending::None;
                if key == Key::Escape {
                    return Ok(EditorAction::Continue);
                }
                return Err(EditorError("digraph character expected".into()));
            }
            Pending::InsertSlide => {
                let action = match key {
                    Key::Ctrl('e') => Some(NativeAction::WindowSlideUp),
                    Key::Ctrl('y') => Some(NativeAction::WindowSlideDown),
                    _ => None,
                };
                let Some(action) = action else {
                    return Err(EditorError("unknown insert window command".into()));
                };
                return self.invoke_native_action(action);
            }
            Pending::VerbatimStart => {
                self.pending = match key {
                    Key::Char('u') => Pending::Verbatim {
                        base: 16,
                        remaining: 4,
                        value: 0,
                        unicode: true,
                    },
                    Key::Char('U') => Pending::Verbatim {
                        base: 16,
                        remaining: 8,
                        value: 0,
                        unicode: true,
                    },
                    Key::Char('x' | 'X') => Pending::Verbatim {
                        base: 16,
                        remaining: 2,
                        value: 0,
                        unicode: false,
                    },
                    Key::Char('o' | 'O') => Pending::Verbatim {
                        base: 8,
                        remaining: 3,
                        value: 0,
                        unicode: false,
                    },
                    Key::Char(c) if c.is_ascii_digit() => Pending::Verbatim {
                        base: 10,
                        remaining: 2,
                        value: c as u32 - '0' as u32,
                        unicode: false,
                    },
                    _ => Pending::None,
                };
                if matches!(self.pending, Pending::None) {
                    let bytes = match key {
                        Key::Char(c) => {
                            let mut bytes = [0; 4];
                            c.encode_utf8(&mut bytes).as_bytes().to_vec()
                        }
                        Key::Tab => vec![b'\t'],
                        Key::Enter => vec![b'\r'],
                        Key::Escape => vec![0x1b],
                        Key::Ctrl(c) if c.is_ascii() => vec![(c as u8) & 0x1f],
                        _ => return Err(EditorError("unsupported verbatim key".into())),
                    };
                    self.insert_literal_bytes(&bytes)?;
                }
                return Ok(EditorAction::Continue);
            }
            Pending::Verbatim {
                base,
                remaining,
                value,
                unicode,
            } => {
                let digit = match key {
                    Key::Char(c) if c.is_ascii() => c.to_digit(base),
                    _ => None,
                };
                if let Some(digit) = digit {
                    let value = value
                        .checked_mul(base)
                        .and_then(|n| n.checked_add(digit))
                        .ok_or_else(|| EditorError("verbatim value overflow".into()))?;
                    if remaining > 1 {
                        self.pending = Pending::Verbatim {
                            base,
                            remaining: remaining - 1,
                            value,
                            unicode,
                        };
                    } else {
                        self.pending = Pending::None;
                        self.emit_verbatim(value, unicode)?;
                    }
                    return Ok(EditorAction::Continue);
                }
                self.pending = Pending::None;
                self.emit_verbatim(value, unicode)?;
                return self.insert_key(key);
            }
            _ => {}
        }
        if matches!(key, Key::Escape | Key::Ctrl('[') | Key::Ctrl('c')) {
            return self.leave_insert_mode(previous_newline);
        }
        if matches!(self.pending, Pending::InsertRegister) {
            self.pending = Pending::None;
            if let Key::Char(c) = key {
                let bytes = self
                    .register(c)
                    .map(|r| r.bytes.clone())
                    .unwrap_or_default();
                self.insert_bytes(&bytes)?;
                return Ok(EditorAction::Continue);
            }
            return Err(EditorError("register expected".into()));
        }
        let key = if key == Key::Ctrl('m') {
            Key::Enter
        } else {
            key
        };
        if self.insert_count > 1 {
            if self.insert_keys.len() >= MAX_REPEAT_KEYS {
                return Err(EditorError("insert repeat limit exceeded".into()));
            }
            self.insert_keys.push(key);
        }
        let data = self.text.to_vec();
        if key == Key::Ctrl('j') {
            self.insert_literal_bytes(b"\n")?;
            return Ok(EditorAction::Continue);
        }
        if key == Key::BackTab {
            self.align_indentation(false)?;
            return Ok(EditorAction::Continue);
        }
        if key == Key::Enter {
            self.insert_newline()?;
            return Ok(EditorAction::Continue);
        }
        if self.input_interception {
            if let Key::Char(c) = key {
                return self.request_input(c);
            }
        }
        if self.selection_set.len() > 1 {
            match key {
                Key::Enter | Key::Tab | Key::Backspace | Key::Delete | Key::Ctrl('w' | 'u') => {
                    let edits = self
                        .selection_set
                        .selections()
                        .iter()
                        .map(|selection| {
                            let cursor = selection.cursor;
                            match key {
                                Key::Enter => {
                                    let mut bytes = vec![b'\n'];
                                    if self.auto_indent {
                                        let start = motion::line_begin(&data, cursor);
                                        bytes.extend_from_slice(
                                            &data[start..motion::line_start(&data, cursor)],
                                        );
                                    }
                                    (cursor..cursor, bytes)
                                }
                                Key::Tab => {
                                    let bytes = if self.expand_tab {
                                        let width = self.tab_width.clamp(1, 8);
                                        vec![
                                            b' ';
                                            width - self.display_column(&data, cursor) % width
                                        ]
                                    } else {
                                        vec![b'\t']
                                    };
                                    (cursor..cursor, bytes)
                                }
                                Key::Delete => {
                                    (cursor..motion::char_next(&data, cursor), Vec::new())
                                }
                                Key::Ctrl('w') => {
                                    (motion::word_start_prev(&data, cursor)..cursor, Vec::new())
                                }
                                Key::Ctrl('u') => {
                                    (motion::line_begin(&data, cursor)..cursor, Vec::new())
                                }
                                _ => (motion::char_prev(&data, cursor)..cursor, Vec::new()),
                            }
                        })
                        .collect();
                    self.commit_multiple(
                        edits,
                        if matches!(key, Key::Enter | Key::Tab) {
                            Placement::End
                        } else {
                            Placement::Start
                        },
                    )?;
                    return Ok(EditorAction::Continue);
                }
                Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End => {
                    let positions = self
                        .selection_set
                        .selections()
                        .iter()
                        .map(|selection| {
                            let cursor = selection.cursor;
                            match key {
                                Key::Left => motion::char_prev(&data, cursor),
                                Key::Right => motion::char_next(&data, cursor),
                                Key::Home => motion::line_begin(&data, cursor),
                                Key::End => motion::line_end(&data, cursor),
                                Key::Up => {
                                    let start =
                                        motion::line_begin(&data, motion::line_prev(&data, cursor));
                                    self.column_position(
                                        &data,
                                        start,
                                        self.display_column(&data, cursor),
                                    )
                                }
                                _ => {
                                    let start = motion::line_next(&data, cursor);
                                    if start >= data.len() {
                                        cursor
                                    } else {
                                        self.column_position(
                                            &data,
                                            start,
                                            self.display_column(&data, cursor),
                                        )
                                    }
                                }
                            }
                        })
                        .collect();
                    self.set_cursors(positions, self.primary_index())?;
                    return Ok(EditorAction::Continue);
                }
                _ => {}
            }
        }
        match key {
            Key::Char(c) => {
                let mut bytes = [0; 4];
                self.insert_bytes(c.encode_utf8(&mut bytes).as_bytes())?;
            }
            Key::Enter => {
                let indent = if self.auto_indent {
                    let start = motion::line_begin(&data, self.cursor);
                    data[start..motion::line_start(&data, self.cursor)].to_vec()
                } else {
                    Vec::new()
                };
                self.insert_bytes(b"\n")?;
                self.insert_bytes(&indent)?;
            }
            Key::Tab => {
                let bytes = if self.expand_tab {
                    let width = self.tab_width.clamp(1, 8);
                    vec![b' '; width - motion::line_width_get(&data, self.cursor) % width]
                } else {
                    vec![b'\t']
                };
                self.insert_bytes(&bytes)?;
            }
            Key::Backspace => {
                let prev = motion::char_prev(&data, self.cursor);
                self.text.delete(prev..self.cursor)?;
                self.cursor = prev;
                self.recording_change = true;
            }
            Key::Delete => {
                self.text
                    .delete(self.cursor..motion::char_next(&data, self.cursor))?;
                self.recording_change = true;
            }
            Key::Left => self.cursor = motion::char_prev(&data, self.cursor),
            Key::Right => self.cursor = motion::char_next(&data, self.cursor),
            Key::Up => self.cursor = motion::line_up(&data, self.cursor),
            Key::Down => self.cursor = motion::line_down(&data, self.cursor),
            Key::Home => self.cursor = motion::line_begin(&data, self.cursor),
            Key::End => self.cursor = motion::line_end(&data, self.cursor),
            // Native readline bindings move the cursor without inserting.
            Key::Ctrl('a') => self.cursor = motion::line_begin(&data, self.cursor),
            Key::Ctrl('e') => self.cursor = motion::line_end(&data, self.cursor),
            Key::ShiftLeft => {
                self.cursor = motion::longword_start_prev(&data, self.cursor);
            }
            Key::ShiftRight => {
                self.cursor = motion::longword_start_next(&data, self.cursor);
            }
            Key::PageUp => return self.invoke_native_action(NativeAction::WindowPageUp),
            Key::PageDown => return self.invoke_native_action(NativeAction::WindowPageDown),
            Key::ShiftPageUp => {
                return self.invoke_native_action(NativeAction::WindowHalfpageUp);
            }
            Key::ShiftPageDown => {
                return self.invoke_native_action(NativeAction::WindowHalfpageDown);
            }
            Key::Ctrl('z') => return self.invoke_native_action(NativeAction::EditorSuspend),
            Key::Ctrl('x') => {
                self.pending = Pending::InsertSlide;
                return Ok(EditorAction::Continue);
            }
            Key::Ctrl('r') => self.pending = Pending::InsertRegister,
            Key::Ctrl('k') => {
                self.pending = Pending::InsertDigraph;
                self.digraph_bytes.clear();
            }
            Key::Ctrl('v') => self.pending = Pending::VerbatimStart,
            Key::Ctrl('t' | 'd') => self.shift_insert(key == Key::Ctrl('t'))?,
            Key::Ctrl('w') => {
                let prev = motion::word_start_prev(&data, self.cursor);
                self.text.delete(prev..self.cursor)?;
                self.cursor = prev;
                self.recording_change = true;
            }
            Key::Ctrl('u') => {
                let prev = motion::line_begin(&data, self.cursor);
                self.text.delete(prev..self.cursor)?;
                self.cursor = prev;
                self.recording_change = true;
            }
            _ => return Err(EditorError("unsupported insert key".into())),
        }
        Ok(EditorAction::Continue)
    }

    fn leave_insert_mode(&mut self, previous_newline: bool) -> Result<EditorAction, EditorError> {
        if self.auto_indent && previous_newline {
            let data = self.text.to_vec();
            let edits: Vec<_> = self
                .selections()
                .iter()
                .filter_map(|s| {
                    let start = motion::line_start(&data, s.cursor);
                    let end = motion::line_end(&data, s.cursor);
                    let begin = motion::line_begin(&data, s.cursor);
                    (start == s.cursor && start == end && begin < start)
                        .then_some((begin..start, Vec::new()))
                })
                .collect();
            if !edits.is_empty() {
                self.commit_multiple(edits, Placement::Start)?;
            }
        }
        let count = std::mem::replace(&mut self.insert_count, 1);
        let keys = std::mem::take(&mut self.insert_keys);
        let open_line = self.open_line.take();
        if keys
            .len()
            .checked_mul(count)
            .is_none_or(|n| n > MAX_REPEAT_KEYS)
        {
            return Err(EditorError("insert repeat limit exceeded".into()));
        }
        for _ in 1..count {
            if let Some(below) = open_line {
                self.insert_replay.push_back(InsertReplayStep::Open(below));
            }
            for key in &keys {
                self.insert_replay.push_back(InsertReplayStep::Key(*key));
            }
        }
        self.insert_replay.push_back(InsertReplayStep::Finish);
        self.drive_insert_replay()
    }
    fn drive_insert_replay(&mut self) -> Result<EditorAction, EditorError> {
        self.driving_insert_replay = true;
        let result = (|| {
            while let Some(step) = self.insert_replay.pop_front() {
                match step {
                    InsertReplayStep::Open(below) => self.open_new_line(below)?,
                    InsertReplayStep::Key(key) => {
                        let action = self.insert_key(key)?;
                        if action != EditorAction::Continue {
                            return Ok(action);
                        }
                    }
                    InsertReplayStep::Finish => {
                        self.insert_keys.clear();
                        self.mode = Mode::Normal;
                        self.pending = Pending::None;
                        self.text.snapshot();
                        if !self.internal_scope
                            && !self.replaying
                            && self.recording_change
                            && self.repeatable
                        {
                            self.last_change = std::mem::take(&mut self.command_keys);
                        }
                        self.recording_change = false;
                        self.command_keys.clear();
                        self.repeatable = true;
                        self.selected_register = None;
                    }
                }
            }
            Ok(EditorAction::Continue)
        })();
        self.driving_insert_replay = false;
        result
    }
    fn insert_bytes(&mut self, bytes: &[u8]) -> Result<(), EditorError> {
        if bytes.len() > MAX_INSERT_BYTES {
            return Err(EditorError("insertion size limit exceeded".into()));
        }
        if self.selection_set.len() > 1 {
            let data = self.text.to_vec();
            let edits = self
                .selection_set
                .selections()
                .iter()
                .map(|selection| {
                    let cursor = selection.cursor;
                    let end = if self.mode == Mode::Replace
                        && data.get(cursor).is_some_and(|b| *b != b'\n')
                    {
                        motion::char_next(&data, cursor)
                    } else {
                        cursor
                    };
                    (cursor..end, bytes.to_vec())
                })
                .collect();
            return self.commit_multiple(edits, Placement::End);
        }
        if self.mode == Mode::Replace && self.text.byte(self.cursor).is_some_and(|b| b != b'\n') {
            let data = self.text.to_vec();
            let end = motion::char_next(&data, self.cursor);
            self.text.replace(self.cursor..end, bytes)?;
        } else {
            self.text.insert(self.cursor, bytes)?;
        }
        self.cursor += bytes.len();
        self.recording_change |= !bytes.is_empty();
        Ok(())
    }

    fn insert_literal_bytes(&mut self, bytes: &[u8]) -> Result<(), EditorError> {
        if bytes.len() > MAX_INSERT_BYTES {
            return Err(EditorError("insertion size limit exceeded".into()));
        }
        if self.selection_set.len() > 1 {
            let edits = self
                .selections()
                .iter()
                .map(|selection| (selection.cursor..selection.cursor, bytes.to_vec()))
                .collect();
            self.commit_multiple(edits, Placement::End)
        } else {
            self.text.insert(self.cursor, bytes)?;
            self.cursor += bytes.len();
            self.recording_change |= !bytes.is_empty();
            Ok(())
        }
    }

    fn emit_verbatim(&mut self, value: u32, unicode: bool) -> Result<(), EditorError> {
        let bytes = if !unicode || value <= 0x7f {
            vec![value as u8]
        } else if value <= 0x7ff {
            vec![(0xc0 | (value >> 6)) as u8, (0x80 | (value & 0x3f)) as u8]
        } else if value <= 0xffff {
            vec![
                (0xe0 | (value >> 12)) as u8,
                (0x80 | ((value >> 6) & 0x3f)) as u8,
                (0x80 | (value & 0x3f)) as u8,
            ]
        } else if value <= 0x10ffff {
            vec![
                (0xf0 | (value >> 18)) as u8,
                (0x80 | ((value >> 12) & 0x3f)) as u8,
                (0x80 | ((value >> 6) & 0x3f)) as u8,
                (0x80 | (value & 0x3f)) as u8,
            ]
        } else {
            Vec::new()
        };
        self.insert_literal_bytes(&bytes)
    }

    fn shift_insert(&mut self, right: bool) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let lines: BTreeSet<_> = self
            .selections()
            .iter()
            .map(|s| motion::line_begin(&data, s.cursor))
            .collect();
        if right {
            let bytes = if self.expand_tab {
                vec![b' '; self.tab_width.clamp(1, 8)]
            } else {
                vec![b'\t']
            };
            return self.insert_preserving_selections(
                lines
                    .into_iter()
                    .map(|line| (line, bytes.clone()))
                    .collect(),
            );
        }
        let edits: Vec<_> = lines
            .into_iter()
            .filter_map(|line| {
                let count = if data.get(line) == Some(&b'\t') {
                    1
                } else {
                    data[line..]
                        .iter()
                        .take(self.tab_width.clamp(1, 8))
                        .take_while(|b| **b == b' ')
                        .count()
                };
                (count > 0).then_some(line..line + count)
            })
            .collect();
        let primary = self.primary_index();
        let saved: Vec<_> = self
            .selections()
            .iter()
            .map(|s| (s.cursor, self.text.mark(s.cursor)))
            .collect();
        for range in edits.iter().rev() {
            self.text.delete(range.clone())?;
        }
        let positions = saved
            .into_iter()
            .map(|(position, mark)| {
                mark.and_then(|mark| self.text.mark_get(mark))
                    .unwrap_or_else(|| {
                        let mut removed = 0;
                        for range in &edits {
                            if range.end <= position {
                                removed += range.len();
                            } else if range.start <= position {
                                return range.start - removed;
                            } else {
                                break;
                            }
                        }
                        position - removed
                    })
            })
            .collect();
        self.set_cursors(positions, primary)?;
        self.recording_change |= !edits.is_empty();
        Ok(())
    }

    fn insert_newline(&mut self) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let mut adjustments = Vec::new();
        let mut append_newline = false;
        let edits = self
            .selection_set
            .selections()
            .iter()
            .map(|selection| {
                let cursor = selection.cursor;
                let mut insertion = cursor;
                let mut preserved_indent = 0;
                let mut bytes = vec![b'\n'];
                if self.auto_indent {
                    let start = motion::line_begin(&data, cursor);
                    let nonblank = motion::line_start(&data, cursor);
                    let limit = nonblank.min(cursor);
                    if limit == motion::line_end(&data, nonblank) {
                        insertion = start;
                        preserved_indent = limit - start;
                    } else {
                        bytes.extend_from_slice(&data[start..limit]);
                    }
                }
                let eof = cursor == data.len();
                if eof && data.last() != Some(&b'\n') {
                    if insertion == cursor {
                        bytes.push(b'\n');
                    } else {
                        append_newline = true;
                    }
                }
                adjustments.push((usize::from(eof && insertion == cursor), preserved_indent));
                (insertion..insertion, bytes)
            })
            .collect();
        self.commit_multiple(edits, Placement::End)?;
        if append_newline {
            self.text.insert(self.text.len(), b"\n")?;
        }
        let positions = self
            .selection_set
            .selections()
            .iter()
            .enumerate()
            .map(|(index, selection)| {
                selection.cursor.saturating_sub(adjustments[index].0) + adjustments[index].1
            })
            .collect();
        self.set_cursors(positions, self.primary_index())?;
        self.last_insert_newline = true;
        Ok(())
    }

    fn open_new_line(&mut self, below: bool) -> Result<(), EditorError> {
        if self.selection_set.len() > 1 {
            return self.open_multiple_lines(below);
        }
        let data = self.text.to_vec();
        let start = motion::line_begin(&data, self.cursor);
        let indent = if self.auto_indent {
            data[start..motion::line_start(&data, self.cursor)].to_vec()
        } else {
            Vec::new()
        };
        let pos = if below {
            motion::line_next(&data, self.cursor)
        } else {
            start
        };
        let mut bytes = Vec::new();
        if below && pos > 0 && data.get(pos - 1) != Some(&b'\n') {
            bytes.push(b'\n');
        }
        let cursor = pos + bytes.len() + indent.len();
        bytes.extend_from_slice(&indent);
        bytes.push(b'\n');
        self.text.insert(pos, &bytes)?;
        self.cursor = cursor;
        self.recording_change = true;
        Ok(())
    }

    fn begin_multi_insert(&mut self, key: Key, count: usize) -> Result<(), EditorError> {
        self.text.snapshot();
        if matches!(key, Key::Char('o' | 'O')) {
            self.open_multiple_lines(key == Key::Char('o'))?;
        } else {
            let data = self.text.to_vec();
            let positions = self
                .selection_set
                .selections()
                .iter()
                .map(|selection| match key {
                    Key::Char('I') => motion::line_start(&data, selection.cursor),
                    Key::Char('A') => motion::line_end(&data, selection.cursor),
                    Key::Char('a') if data.get(selection.cursor).is_some_and(|b| *b != b'\n') => {
                        motion::char_next(&data, selection.cursor)
                    }
                    _ => selection.cursor,
                })
                .collect();
            self.set_cursors(positions, self.primary_index())?;
        }
        self.enter_insert(key == Key::Char('R'), count);
        self.open_line = match key {
            Key::Char('o') => Some(true),
            Key::Char('O') => Some(false),
            _ => None,
        };
        Ok(())
    }

    fn open_multiple_lines(&mut self, below: bool) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let edits = self
            .selection_set
            .selections()
            .iter()
            .map(|selection| {
                let cursor = selection.cursor;
                let start = motion::line_begin(&data, cursor);
                let pos = if below {
                    motion::line_next(&data, cursor)
                } else {
                    start
                };
                let mut bytes = Vec::new();
                if below && pos > 0 && data.get(pos - 1) != Some(&b'\n') {
                    bytes.push(b'\n');
                }
                if self.auto_indent {
                    bytes.extend_from_slice(&data[start..motion::line_start(&data, cursor)]);
                }
                bytes.push(b'\n');
                (pos..pos, bytes)
            })
            .collect();
        self.commit_multiple(edits, Placement::End)?;
        // The new cursor is before the newline we just created.
        let positions = self
            .selection_set
            .selections()
            .iter()
            .map(|s| s.cursor.saturating_sub(1))
            .collect();
        self.set_cursors(positions, self.primary_index())
    }

    fn put(&mut self, after: bool) -> Result<EditorAction, EditorError> {
        let count = self.take_count();
        let visual = self.is_visual();
        let register_name = self.selected_register;
        if visual {
            self.window_selection_save()?;
        }
        if self.selection_set.len() > 1 {
            let action = self.put_multiple(after, count)?;
            if visual {
                self.record_visual_put_repeat(after, count, register_name);
            }
            return Ok(action);
        }
        let register = self
            .register(self.operation_register_name())
            .cloned()
            .unwrap_or_default();
        // The native put operator deletes the visual selection before it
        // inserts, so an empty register still removes the selected text.
        let selection = self.selection().filter(|range| !range.is_empty());
        if register
            .bytes
            .len()
            .checked_mul(count)
            .is_none_or(|n| n > MAX_INSERT_BYTES)
        {
            return Err(EditorError("put size limit exceeded".into()));
        }
        let data = self.text.to_vec();
        let source_cursor = selection.as_ref().map_or(self.cursor, |range| range.start);
        let mut pos = self.cursor;
        self.text.snapshot();
        if let Some(range) = selection.clone() {
            // op_put: a linewise register over a non-linewise selection
            // continues on the following line, like the native PUT_AFTER
            // adjustment guarded by text_range_is_linewise.
            let sel_linewise = !range.is_empty()
                && motion::line_begin(&data, range.start) == range.start
                && motion::line_begin(&data, range.end) == range.end;
            pos = range.start;
            self.text.delete(range)?;
            if register.linewise && !sel_linewise {
                let current = self.text.to_vec();
                pos = motion::line_next(&current, pos);
            }
        } else if register.linewise {
            pos = if after {
                motion::line_next(&data, pos)
            } else {
                motion::line_begin(&data, pos)
            };
        } else if after && data.get(pos).is_some_and(|b| *b != b'\n') {
            pos = motion::char_next(&data, pos);
        }
        for _ in 0..count {
            if register.linewise && pos > 0 && self.text.byte(pos - 1) != Some(b'\n') {
                self.text.insert(pos, b"\n")?;
                pos += 1;
            }
            self.text.insert(pos, &register.bytes)?;
            pos += register.bytes.len();
            if register.linewise && pos > 0 && self.text.byte(pos - 1) != Some(b'\n') {
                self.text.insert(pos, b"\n")?;
                pos += 1;
            }
        }
        let data = self.text.to_vec();
        self.cursor = if register.linewise {
            let position = if after {
                motion::line_next(&data, source_cursor)
            } else {
                source_cursor
            };
            motion::line_start(&data, position)
        } else {
            motion::char_prev(&data, pos)
        };
        self.mode = Mode::Normal;
        self.text.snapshot();
        self.recording_change = true;
        if visual {
            self.record_visual_put_repeat(after, count, register_name);
        }
        Ok(EditorAction::Continue)
    }

    fn record_visual_put_repeat(&mut self, after: bool, count: usize, register: Option<char>) {
        if self.internal_scope || self.replaying || !self.repeatable {
            return;
        }
        let mut keys = Vec::new();
        if let Some(register) = register {
            keys.extend([Key::Char('"'), Key::Char(register)]);
        }
        if count > 1 {
            keys.extend(count.to_string().chars().map(Key::Char));
        }
        keys.push(Key::Char(if after { 'p' } else { 'P' }));
        self.last_change = keys;
        self.recording_change = false;
    }

    fn put_multiple(&mut self, after: bool, count: usize) -> Result<EditorAction, EditorError> {
        let name = self.operation_register_name().to_ascii_lowercase();
        let default = self.register(name).cloned().unwrap_or_default();
        let slots = self.register_slots.get(&name).cloned().unwrap_or_default();
        let data = self.text.to_vec();
        let visual = self.is_visual();
        let ranges = self.selection_ranges();
        let mut lengths = Vec::new();
        let mut linewise = Vec::new();
        let mut edits = Vec::new();
        for (index, selection) in self.selection_set.selections().iter().enumerate() {
            let register = slots
                .get(index)
                .or_else(|| {
                    if slots.len() == 1 {
                        slots.first()
                    } else {
                        None
                    }
                })
                .unwrap_or(&default);
            if register
                .bytes
                .len()
                .checked_mul(count)
                .is_none_or(|n| n > MAX_INSERT_BYTES)
            {
                return Err(EditorError("put size limit exceeded".into()));
            }
            let mut pos = selection.cursor;
            let range = if visual {
                ranges[index].clone()
            } else {
                if register.linewise {
                    pos = if after {
                        motion::line_next(&data, pos)
                    } else {
                        motion::line_begin(&data, pos)
                    };
                } else if after && data.get(pos).is_some_and(|b| *b != b'\n') {
                    pos = motion::char_next(&data, pos);
                }
                pos..pos
            };
            let mut bytes = Vec::new();
            if register.linewise && range.start > 0 && data.get(range.start - 1) != Some(&b'\n') {
                bytes.push(b'\n');
            }
            for _ in 0..count {
                bytes.extend_from_slice(&register.bytes);
                if register.linewise && !register.bytes.is_empty() && bytes.last() != Some(&b'\n') {
                    bytes.push(b'\n');
                }
            }
            lengths.push(bytes.len());
            linewise.push(register.linewise);
            edits.push((range, bytes));
        }
        self.text.snapshot();
        self.mode = Mode::Normal;
        self.commit_multiple(edits, Placement::End)?;
        let data = self.text.to_vec();
        let positions = self
            .selection_set
            .selections()
            .iter()
            .enumerate()
            .map(|(index, selection)| {
                if linewise[index] {
                    motion::line_start(&data, selection.cursor - lengths[index])
                } else {
                    motion::char_prev(&data, selection.cursor)
                }
            })
            .collect();
        self.set_cursors(positions, self.primary_index())?;
        self.text.snapshot();
        Ok(EditorAction::Continue)
    }

    fn replace_character(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        let replacement = match key {
            Key::Char(c) => {
                let mut b = [0; 4];
                c.encode_utf8(&mut b).as_bytes().to_vec()
            }
            Key::Enter => vec![b'\n'],
            _ => return Err(EditorError("replacement character expected".into())),
        };
        let data = self.text.to_vec();
        let count = self.take_count();
        if self.selection_set.len() > 1 {
            let visual = self.is_visual();
            let ranges = if visual {
                self.selection_ranges()
            } else {
                self.selection_set
                    .selections()
                    .iter()
                    .map(|selection| {
                        let mut end = selection.cursor;
                        for _ in 0..count {
                            end = motion::char_next(&data, end);
                        }
                        selection.cursor..end
                    })
                    .collect()
            };
            let edits = ranges
                .into_iter()
                .map(|range| {
                    let mut pos = range.start;
                    let mut count = 0;
                    while pos < range.end {
                        pos = motion::char_next(&data, pos);
                        count += 1;
                    }
                    (range, replacement.repeat(count))
                })
                .collect();
            self.text.snapshot();
            self.mode = Mode::Normal;
            self.commit_multiple(edits, Placement::Start)?;
            self.text.snapshot();
            return Ok(EditorAction::Continue);
        }
        let range = self.selection().unwrap_or_else(|| {
            let mut end = self.cursor;
            for _ in 0..count {
                end = motion::char_next(&data, end);
            }
            self.cursor..end
        });
        let end = range.end;
        let chars = {
            let mut p = range.start;
            let mut n = 0;
            while p < end {
                p = motion::char_next(&data, p);
                n += 1;
            }
            n
        };
        let bytes = replacement.repeat(chars);
        self.text.snapshot();
        self.text.replace(range.clone(), &bytes)?;
        self.cursor = range.start;
        self.mode = Mode::Normal;
        self.text.snapshot();
        self.recording_change = true;
        Ok(EditorAction::Continue)
    }

    fn join(&mut self) -> Result<EditorAction, EditorError> {
        self.join_with_space(true)
    }

    fn join_with_space(&mut self, insert_space: bool) -> Result<EditorAction, EditorError> {
        let count = self.take_count().saturating_sub(1).max(1);
        let visual = self.is_visual();
        let linewise = self.mode == Mode::VisualLine;
        if visual {
            self.window_selection_save()?;
        }
        let ranges = visual.then(|| self.selection_ranges());
        self.run_line_operator(ranges, |plan, range, cursor| {
            let data = plan.text.to_vec();
            let range = match range {
                Some(range) if linewise => motion::range_linewise(&data, range),
                Some(range) => range,
                None => {
                    let mut end = cursor;
                    for _ in 0..count {
                        let next = motion::line_next(&data, end);
                        if next == end {
                            break;
                        }
                        end = next;
                    }
                    cursor..end
                }
            };
            plan.join(range, linewise, insert_space)
        })?;
        Ok(EditorAction::Continue)
    }

    fn find_character(&self, find: Find, count: usize) -> (usize, bool) {
        let data = self.text.to_vec();
        let mut bytes = [0; 4];
        let needle = find.character.encode_utf8(&mut bytes).as_bytes();
        let mut pos = self.cursor;
        if find.cross_line {
            for _ in 0..count {
                pos = Self::cross_line_target(&data, pos, needle, find);
            }
            return (pos, !find.backwards);
        }
        for _ in 0..count {
            let found = if find.backwards {
                motion::line_find_prev(&data, pos.saturating_sub(usize::from(find.till)), needle)
            } else {
                motion::line_find_next(
                    &data,
                    pos.saturating_add(if find.till { 2 } else { 1 })
                        .min(data.len()),
                    needle,
                )
            };
            if let Some(found) = found {
                pos = if find.till {
                    if find.backwards {
                        motion::char_next(&data, found)
                    } else {
                        motion::char_prev(&data, found)
                    }
                } else {
                    found
                };
            } else {
                return (self.cursor, !find.backwards);
            }
        }
        (pos, !find.backwards)
    }

    /// Native `to_left`/`to_right`/`till_left`/`till_right`: search the whole
    /// document, starting one byte past the cursor (two when till) and clamping
    /// the result by one character. The original position is kept on failure.
    fn cross_line_target(data: &[u8], pos: usize, needle: &[u8], find: Find) -> usize {
        if find.backwards {
            let from = if find.till {
                pos.saturating_sub(1)
            } else {
                pos
            };
            match motion::find_prev(data, from, needle) {
                Some(hit) if find.till => motion::char_next(data, hit),
                Some(hit) => hit,
                None => pos,
            }
        } else {
            let from = pos
                .saturating_add(if find.till { 2 } else { 1 })
                .min(data.len());
            match motion::find_next(data, from, needle) {
                Some(hit) if data.get(hit) != Some(&needle[0]) => pos,
                Some(hit) if find.till => motion::char_prev(data, hit),
                Some(hit) => hit,
                None => pos,
            }
        }
    }

    fn history_move(&mut self, redo: bool, count: usize) -> Result<(), EditorError> {
        self.mode = Mode::Normal;
        for _ in 0..count {
            let primary = self.selection_set.primary_id();
            let views = self.view_identified();
            let marks: Vec<_> = views
                .iter()
                .map(|(id, _)| (*id, self.selection_marks(*id)))
                .collect();
            let moved = if redo {
                self.text.redo()
            } else {
                self.text.undo()
            };
            let Some(pos) = moved else {
                break;
            };
            if views.len() == 1 {
                self.set_cursors(vec![pos.min(self.text.len())], 0)?;
            } else {
                let identified = views
                    .into_iter()
                    .zip(&marks)
                    .map(|((id, fallback), (_, mark))| {
                        let cursor = mark
                            .as_ref()
                            .and_then(|m| m.cursor_position(&self.text))
                            .unwrap_or(fallback.cursor.min(self.text.len()));
                        (
                            id,
                            Selection {
                                anchor: cursor,
                                cursor,
                            },
                        )
                    })
                    .collect();
                self.selection_set =
                    SelectionSet::from_identified(&self.text.to_vec(), identified, primary)
                        .map_err(|e| EditorError(e.to_string()))?;
                // C keeps the selection objects and their byte marks. A cursor
                // on inserted join whitespace becomes invalid after undo and
                // becomes valid again when that exact revision is redone.
                self.external_marks = marks
                    .into_iter()
                    .filter_map(|(id, mark)| mark.map(|m| (id, m)))
                    .collect();
                self.sync_set_to_primary();
            }
        }
        Ok(())
    }

    fn find_targets(&mut self, find: Find, count: usize) -> (usize, bool) {
        if self.selection_set.len() == 1 {
            return self.find_character(find, count);
        }
        let original = self.cursor;
        let mut primary = (self.cursor, !find.backwards);
        self.motion_targets.clear();
        let selections = self.selection_set.selections().to_vec();
        for (index, selection) in selections.into_iter().enumerate() {
            self.cursor = selection.cursor;
            let (cursor, inclusive) = self.find_character(find, count);
            self.motion_targets.push(Selection {
                anchor: if self.is_visual() {
                    selection.anchor
                } else {
                    cursor
                },
                cursor,
            });
            if index == self.primary_index() {
                primary = (cursor, inclusive);
            }
        }
        self.cursor = original;
        primary
    }

    fn show_prompt(&mut self, prompt: Prompt) -> Result<EditorAction, EditorError> {
        if self.host_prompts {
            return Ok(EditorAction::PromptShow {
                title: vec![match prompt {
                    Prompt::Command => b':',
                    Prompt::Search { backwards: false } => b'/',
                    Prompt::Search { backwards: true } => b'?',
                }],
            });
        }
        self.begin_prompt(prompt);
        Ok(EditorAction::Continue)
    }
    fn begin_prompt(&mut self, prompt: Prompt) {
        self.prompt_insert = true;
        self.prompt_mode = self.mode;
        self.history_index = None;
        self.history_draft.clear();
        self.prompt_range = self.selection();
        self.prompt_ranges = if self.is_visual() {
            self.selection_ranges()
        } else {
            self.selection_set
                .selections()
                .iter()
                .map(|selection| selection.cursor..selection.cursor)
                .collect()
        };
        self.prompt_count = self.take_count();
        self.prompt = prompt;
        self.command.clear();
        self.mode = Mode::Command;
        self.count = None;
    }

    fn prompt_key(&mut self, key: Key) -> Result<EditorAction, EditorError> {
        let mut key =
            if matches!(self.pending, Pending::None) && matches!(key, Key::Ctrl('j' | 'm')) {
                Key::Enter
            } else {
                key
            };
        if !self.prompt_insert {
            if matches!(self.pending, Pending::MacroPlay) {
                self.pending = Pending::None;
                return match key {
                    Key::Char(':') if self.register(':').is_none_or(|r| r.bytes.is_empty()) => {
                        Ok(EditorAction::Continue)
                    }
                    _ => Err(EditorError(
                        "macro execution in a modal prompt is not implemented yet".into(),
                    )),
                };
            }
            match key {
                Key::Escape | Key::Ctrl('[') | Key::Ctrl('c') | Key::Enter => {}
                Key::Char('i' | 'a' | 'I' | 'A') => {
                    self.prompt_insert = true;
                    return Ok(EditorAction::Continue);
                }
                Key::Char('j') => key = Key::Down,
                Key::Char('k') => key = Key::Up,
                Key::Char('@') => {
                    self.pending = Pending::MacroPlay;
                    return Ok(EditorAction::Continue);
                }
                Key::Up | Key::Down => {}
                _ => return Err(EditorError("unsupported modal prompt key".into())),
            }
        }
        if matches!(self.pending, Pending::InsertRegister) {
            self.pending = Pending::None;
            if let Key::Char(name) = key {
                if let Some(register) = self.register(name) {
                    let text = std::str::from_utf8(&register.bytes)
                        .map_err(|_| EditorError("prompt register is not UTF-8".into()))?
                        .to_owned();
                    self.command.push_str(&text);
                }
                return Ok(EditorAction::Continue);
            }
            return Err(EditorError("prompt register expected".into()));
        }
        match key {
            Key::Escape | Key::Ctrl('[') | Key::Ctrl('c') => {
                if self.prompt_insert {
                    self.prompt_insert = false;
                    self.pending = Pending::None;
                    return Ok(EditorAction::Continue);
                }
                self.command.clear();
                self.mode = if self.prompt_mode == Mode::OperatorPending {
                    Mode::Normal
                } else {
                    self.prompt_mode
                };
                self.pending = Pending::None;
                self.prompt_range = None;
                self.prompt_ranges.clear();
                self.count = None;
                self.operator = None;
            }
            Key::Backspace => {
                if self.command.pop().is_none() {
                    self.cancel();
                }
            }
            Key::Char(c) => self.command.push(c),
            Key::Tab => self.command.push('\t'),
            Key::Up | Key::Down => {
                let history = self
                    .history
                    .get(&self.prompt_prefix())
                    .cloned()
                    .unwrap_or_default();
                if self.history_index.is_none() {
                    self.history_draft = self.command.clone();
                }
                let current = self.history_index.unwrap_or(history.len());
                let index = if key == Key::Up {
                    current.saturating_sub(1)
                } else {
                    current.saturating_add(1).min(history.len())
                };
                self.command = history
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| self.history_draft.clone());
                self.history_index = Some(index);
            }
            Key::Ctrl('r') => self.pending = Pending::InsertRegister,
            Key::Ctrl('u') => self.command.clear(),
            Key::Ctrl('w') => {
                let pos = motion::word_start_prev(self.command.as_bytes(), self.command.len());
                self.command.truncate(pos);
            }
            Key::Enter => {
                self.mode = Mode::Normal;
                let command = std::mem::take(&mut self.command);
                if !command.is_empty() {
                    self.history
                        .entry(self.prompt_prefix())
                        .or_default()
                        .push(command.clone());
                }
                match self.prompt {
                    Prompt::Search { backwards } => {
                        if !command.is_empty() {
                            self.word_search = None;
                            self.search_regex(&command)?;
                            self.session
                                .set_previous_regex(&command)
                                .map_err(|e| EditorError(e.to_string()))?;
                            self.search = command;
                            self.registers.insert(
                                '/',
                                Register {
                                    bytes: self.search.as_bytes().to_vec(),
                                    linewise: false,
                                },
                            );
                        }
                        self.search_backwards = backwards;
                        if matches!(self.prompt_mode, Mode::Visual | Mode::VisualLine) {
                            self.mode = self.prompt_mode;
                        }
                        let origin = self.cursor;
                        for _ in 0..self.prompt_count {
                            self.search_motion(backwards)?;
                        }
                        if self.operator.is_some() {
                            let target = self.cursor;
                            self.cursor = origin;
                            return self.finish_motion(target, false, false);
                        }
                        self.prompt_range = None;
                        self.prompt_ranges.clear();
                    }
                    Prompt::Command => return self.execute_command(&command),
                }
            }
            _ => return Err(EditorError("unsupported prompt key".into())),
        }
        Ok(EditorAction::Continue)
    }

    fn search_motion(&mut self, backwards: bool) -> Result<(), EditorError> {
        if let Some(word) = &self.word_search {
            let data = self.text.to_vec();
            let found = if backwards {
                motion::word_find_prev(&data, self.cursor, word)
                    .or_else(|| motion::word_find_prev(&data, data.len(), word))
            } else {
                motion::word_find_next(&data, motion::char_next(&data, self.cursor), word)
                    .or_else(|| motion::word_find_next(&data, 0, word))
            };
            self.cursor = found
                .ok_or_else(|| EditorError("word not found".into()))?
                .start;
            return Ok(());
        }
        if self.search.is_empty() {
            return Err(EditorError("no previous search".into()));
        }
        let regex = self.search_regex(&self.search)?;
        let data = self.text.to_vec();
        let mut found = None;
        if backwards {
            let mut pos = 0;
            while pos <= data.len() {
                let next = regex
                    .find_with_flags(&data, pos..data.len(), pos > 0, false)
                    .map_err(|e| EditorError(e.to_string()))?;
                let Some(next) = next else {
                    break;
                };
                if next.range.start < self.cursor {
                    found = Some(next.range.start);
                } else if found.is_some() {
                    break;
                }
                let advance = motion::char_next(&data, next.range.start);
                if advance <= pos {
                    break;
                }
                pos = advance;
            }
            if found.is_none() {
                let mut pos = 0;
                while pos <= data.len() {
                    let next = regex
                        .find_with_flags(&data, pos..data.len(), pos > 0, false)
                        .map_err(|e| EditorError(e.to_string()))?;
                    let Some(next) = next else {
                        break;
                    };
                    found = Some(next.range.start);
                    let advance = motion::char_next(&data, next.range.start);
                    if advance <= pos {
                        break;
                    }
                    pos = advance;
                }
            }
        } else {
            let start = motion::char_next(&data, self.cursor);
            found = regex
                .find_with_flags(&data, start..data.len(), start > 0, false)
                .map_err(|e| EditorError(e.to_string()))?
                .map(|m| m.range.start);
            if found.is_none() {
                found = regex
                    .find(&data, 0..data.len())
                    .map_err(|e| EditorError(e.to_string()))?
                    .map(|m| m.range.start);
            }
        }
        self.cursor = found.ok_or_else(|| EditorError("pattern not found".into()))?;
        Ok(())
    }

    fn escape_pattern(bytes: &[u8]) -> String {
        let mut pattern = String::new();
        for c in String::from_utf8_lossy(bytes).chars() {
            if matches!(
                c,
                '.' | '[' | ']' | '(' | ')' | '{' | '}' | '*' | '+' | '?' | '|' | '^' | '$' | '\\'
            ) {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern
    }

    fn search_object(&mut self, backwards: bool) -> Result<EditorAction, EditorError> {
        if self.search.is_empty() {
            return Err(EditorError("no previous search".into()));
        }
        let data = self.text.to_vec();
        let regex = self.search_regex(&self.search)?;
        let count = self.combined_count()?;
        let mut ranges = Vec::new();
        for selection in self.selections() {
            let mut pos = selection.cursor;
            let mut found = None;
            for _ in 0..count {
                if backwards {
                    let mut scan = 0;
                    let mut prior = None;
                    while scan <= pos.min(data.len()) {
                        let next = regex
                            .find_with_flags(
                                &data,
                                scan..data.len(),
                                scan > 0 && data.get(scan - 1) != Some(&b'\n'),
                                false,
                            )
                            .map_err(|e| EditorError(e.to_string()))?;
                        let Some(next) = next else {
                            break;
                        };
                        if next.range.end > pos {
                            break;
                        }
                        prior = Some(next.range.clone());
                        let advance = motion::char_next(&data, next.range.start);
                        if advance <= scan {
                            break;
                        }
                        scan = advance;
                    }
                    found = prior;
                    pos = found.as_ref().map_or(pos, |range| range.start);
                } else {
                    found = regex
                        .find_with_flags(
                            &data,
                            pos..data.len(),
                            pos > 0 && data.get(pos - 1) != Some(&b'\n'),
                            false,
                        )
                        .map_err(|e| EditorError(e.to_string()))?
                        .map(|m| m.range);
                    pos = found
                        .as_ref()
                        .map_or(pos, |range| range.end.max(range.start + 1).min(data.len()));
                }
            }
            ranges.push(found.ok_or_else(|| EditorError("search match not found".into()))?);
        }
        if self.operator.is_some() {
            if ranges.len() > 1 {
                self.apply_many_operators(ranges, false)
            } else {
                self.apply_operator(ranges[0].clone(), false)
            }
        } else {
            self.mode = Mode::Visual;
            self.adopt_ranges(ranges)?;
            Ok(EditorAction::Continue)
        }
    }

    /// Execute a completed ':' prompt. File operations are requests to the host.
    /// Prepare an interactive command, reporting accepted handler failures as
    /// messages while retaining hard parser and transcript errors.
    pub fn prepare_command(&mut self, command: &str) -> Result<EditorAction, EditorError> {
        self.execute_command_with_policy(command, true)
    }
    pub fn execute_command(&mut self, command: &str) -> Result<EditorAction, EditorError> {
        self.execute_command_with_policy(command, false)
    }
    /// The native window scoped options are applied by the host, which owns
    /// the window display. This returns the host command for them.
    fn window_option_command(arg: &str) -> Option<String> {
        let mut words = arg.split_whitespace();
        let first = words.next()?;
        let (raw, inline) = match first.split_once('=') {
            Some((name, value)) => (name, Some(value.to_string())),
            None => (first, None),
        };
        let name = raw.trim_end_matches('!');
        if !matches!(
            name,
            "statusbar"
                | "sb"
                | "showeof"
                | "oneline"
                | "numbers"
                | "nu"
                | "relativenumbers"
                | "rnu"
                | "numberwidth"
                | "nuw"
                | "cursorline"
                | "cul"
                | "colorcolumn"
                | "cc"
                | "showspaces"
                | "showtabs"
                | "shownewlines"
                | "wrapcolumn"
                | "wc"
                | "breakat"
                | "brk"
        ) {
            return None;
        }
        let mut command = format!("set {name}");
        if let Some(value) = inline {
            command.push(' ');
            command.push_str(&value);
        } else if let Some(value) = words.next() {
            command.push(' ');
            command.push_str(value);
        }
        Some(command)
    }

    fn execute_command_with_policy(
        &mut self,
        command: &str,
        compatible: bool,
    ) -> Result<EditorAction, EditorError> {
        let command = command.trim();
        self.registers.insert(
            ':',
            Register {
                bytes: command.as_bytes().to_vec(),
                linewise: false,
            },
        );
        let (name, arg) = command
            .split_once(char::is_whitespace)
            .unwrap_or((command, ""));
        if name.trim_end_matches('!') == "set"
            && arg.split_whitespace().any(|word| {
                let option = word.split(['=', '?']).next().unwrap_or(word);
                self.external_options.contains(option)
                    || option
                        .strip_prefix("no")
                        .is_some_and(|option| self.external_options.contains(option))
            })
        {
            return Ok(EditorAction::HostCommand(command.to_owned()));
        }
        if self.external_commands.contains(name.trim_end_matches('!')) {
            return Ok(EditorAction::HostCommand(command.to_owned()));
        }
        let force = name.ends_with('!');
        let name = name.trim_end_matches('!');
        if self.mode == Mode::Normal && self.prompt_range.is_none() && self.selection_set.len() == 1
        {
            if let Ok(line) = command.parse::<usize>() {
                let data = self.text.to_vec();
                self.cursor = motion::line_start(&data, Self::line_number(&data, line));
                self.sync_primary_to_set()?;
                return Ok(EditorAction::Continue);
            }
        }
        let write_visual = self.prompt_range.is_some() || self.is_visual();
        let write_dots = if !self.prompt_ranges.is_empty() {
            self.prompt_ranges.clone()
        } else if self.is_visual() {
            self.selection_ranges()
        } else {
            self.selections()
                .iter()
                .map(|selection| selection.cursor..selection.cursor)
                .collect()
        };
        let write = self
            .session
            .resolve_write(&self.text, command, &write_dots, write_visual)
            .map_err(|e| EditorError(e.to_string()))?;
        if let Some(write) = write {
            if write.visual {
                self.mode = if self.prompt_mode == Mode::VisualLine {
                    Mode::VisualLine
                } else {
                    Mode::Visual
                };
                if let Some(ranges) = &write.ranges {
                    self.adopt_ranges(ranges.clone())?;
                }
            }
            self.prompt_range = None;
            self.prompt_ranges.clear();
            return Ok(EditorAction::Save {
                quit: write.quit,
                force: write.force,
                path: write.path,
                ranges: write.ranges,
                visual: write.visual,
            });
        }
        match name {
            "map" | "unmap" => {
                return Ok(match self.configure_mapping(name == "unmap", force, arg) {
                    Ok(()) => EditorAction::Continue,
                    Err(error) if compatible => EditorAction::Message(error.to_string()),
                    Err(error) => return Err(error),
                });
            }
            "e" | "edit" | "open" | "o" | "split" | "vsplit" | "new" | "vnew" | "b" | "B" | "D"
            | "bdelete" | "buffer" | "cd" | "chdir" | "pwd" | "ls" | "files" | "wincmd"
            | "help" => {
                return Ok(EditorAction::HostCommand(command.to_owned()));
            }
            "qall" => return Ok(EditorAction::HostCommand(command.to_owned())),
            "q" | "quit" => return Ok(EditorAction::Quit { force }),
            "set" => {
                // The window scoped options belong to the host, which owns the
                // window display.
                if let Some(command) = Self::window_option_command(arg) {
                    return Ok(EditorAction::HostCommand(command));
                }
                let result = (|| -> Result<(), EditorError> {
                    // Native `set option [value]`: the option name comes first
                    // and an option that takes a value reads the next word.
                    let mut tokens = arg.split_whitespace().peekable();
                    while let Some(option) = tokens.next() {
                        let (name, inline) = match option.split_once('=') {
                            Some((name, value)) => (name, Some(value.to_string())),
                            None => (option, None),
                        };
                        let boolean = |value: Option<&str>| -> Result<bool, EditorError> {
                            match value {
                                None => Ok(true),
                                Some("1" | "true" | "yes" | "on") => Ok(true),
                                Some("0" | "false" | "no" | "off") => Ok(false),
                                Some(_) => Err(EditorError("expected boolean option value".into())),
                            }
                        };
                        match name {
                            "expandtab" | "et" => self.expand_tab = boolean(inline.as_deref())?,
                            "noexpandtab" | "noet" => self.expand_tab = false,
                            "ignorecase" | "ic" => self.ignorecase = boolean(inline.as_deref())?,
                            "noignorecase" | "noic" => self.ignorecase = false,
                            "change256colors" => self.change256colors = boolean(inline.as_deref())?,
                            "loadmethod" => {
                                let method = match inline {
                                    Some(value) => value,
                                    None => tokens
                                        .next()
                                        .ok_or_else(|| {
                                            EditorError("Expecting string option value".into())
                                        })?
                                        .to_string(),
                                };
                                self.set_loadmethod(Some(method));
                            }
                            "autoindent" | "ai" => self.auto_indent = boolean(inline.as_deref())?,
                            "noautoindent" | "noai" => self.auto_indent = false,
                            "tabwidth" | "tw" => {
                                let value = match inline {
                                    Some(value) => value,
                                    None => tokens
                                        .next()
                                        .ok_or_else(|| {
                                            EditorError("tab width must be 1..8".into())
                                        })?
                                        .to_string(),
                                };
                                self.tab_width = value
                                    .parse::<usize>()
                                    .ok()
                                    .filter(|n| (1..=8).contains(n))
                                    .ok_or_else(|| EditorError("tab width must be 1..8".into()))?;
                            }
                            "shell" => {
                                let value = match inline {
                                    Some(value) => value,
                                    None => tokens
                                        .next()
                                        .ok_or_else(|| {
                                            EditorError("Expecting string option value".into())
                                        })?
                                        .to_string(),
                                };
                                self.set_shell(Some(value));
                            }
                            "escdelay" => {
                                let value = match inline {
                                    Some(value) => value,
                                    None => tokens
                                        .next()
                                        .ok_or_else(|| {
                                            EditorError("escape delay must be a number".into())
                                        })?
                                        .to_string(),
                                };
                                let milliseconds = value.parse::<u64>().map_err(|_| {
                                    EditorError("escape delay must be a number".into())
                                })?;
                                self.set_escape_delay(Some(std::time::Duration::from_millis(
                                    milliseconds,
                                )));
                            }
                            _ => {
                                return Err(EditorError(format!("Unknown option: `{name}`")));
                            }
                        }
                    }
                    Ok(())
                })();
                return Ok(match result {
                    Ok(()) => EditorAction::Continue,
                    Err(error) if compatible => EditorAction::Message(error.to_string()),
                    Err(error) => return Err(error),
                });
            }
            "langmap" => {
                let words =
                    crate::workspace::command_words(arg).map_err(|e| EditorError(e.to_string()))?;
                return match self.configure_langmap(&words) {
                    Ok(()) => Ok(EditorAction::Continue),
                    Err(error) if compatible => Ok(EditorAction::Message(error.to_string())),
                    Err(error) => Err(error),
                };
            }
            "earlier" | "later" => {
                return match crate::history::execute(&mut self.text, name == "earlier", arg) {
                    Ok(position) => {
                        self.cursor = position.unwrap_or(self.cursor).min(self.text.len());
                        let state = self.text.state();
                        Ok(EditorAction::Message(format!(
                            "State from {}",
                            crate::time::local_hm(state)
                        )))
                    }
                    Err(error) if compatible && !error.is_parse() => {
                        Ok(EditorAction::Message(error.to_string()))
                    }
                    Err(error) => Err(EditorError(error.to_string())),
                };
            }
            _ => {}
        }
        let range = self
            .prompt_range
            .take()
            .or_else(|| self.selection())
            .unwrap_or(self.cursor..self.cursor);
        let was_visual = !range.is_empty();
        let ranges = if self.prompt_ranges.is_empty() {
            if self.is_visual() {
                self.selection_ranges()
            } else {
                self.selection_set
                    .selections()
                    .iter()
                    .map(|selection| selection.cursor..selection.cursor)
                    .collect()
            }
        } else {
            std::mem::take(&mut self.prompt_ranges)
        };
        let primary = self.primary_index();
        let original_cursor = self.cursor;
        let cursor_marks: Vec<_> = self
            .selection_set
            .selections()
            .iter()
            .map(|selection| self.text.mark(selection.cursor))
            .collect();
        let ranges = if ranges.is_empty() {
            vec![range]
        } else {
            ranges
        };
        let original_ids = self.selection_ids().to_vec();
        let result = self.session.execute_many(&mut self.text, command, &ranges);
        self.sync_sam_registers();
        let execution = match result {
            Ok(execution) => execution,
            Err(error) if compatible && error.is_evaluation() => {
                return Ok(EditorAction::Message(error.to_string()));
            }
            Err(error) => return Err(EditorError(error.to_string())),
        };
        if !execution.selections.is_empty() {
            self.mode = if execution.selections.iter().all(Range::is_empty) {
                Mode::Normal
            } else {
                Mode::Visual
            };
            self.adopt_sam_ranges(
                execution.selections,
                &execution.selection_inputs,
                &execution.selection_detached,
                &original_ids,
            )?;
        } else if was_visual {
            let ranges: Vec<_> = execution
                .changes
                .iter()
                .filter(|change| change.insertion)
                .map(|change| change.range.clone())
                .collect();
            if !ranges.is_empty() {
                self.mode = Mode::Visual;
                let inputs: Vec<_> = execution
                    .changes
                    .iter()
                    .filter(|c| c.insertion)
                    .map(|c| c.input)
                    .collect();
                let detached: Vec<_> = execution
                    .changes
                    .iter()
                    .filter(|c| c.insertion)
                    .map(|c| c.detached)
                    .collect();
                self.adopt_sam_ranges(ranges, &inputs, &detached, &original_ids)?;
            } else {
                self.mode = Mode::Normal;
                self.cursor = execution
                    .changes
                    .first()
                    .map_or(self.cursor, |change| change.range.start)
                    .min(self.text.len());
                let id = self.selection_set.primary_id();
                self.selection_set = SelectionSet::from_identified(
                    &self.text.to_vec(),
                    vec![(
                        id,
                        Selection {
                            anchor: self.cursor,
                            cursor: self.cursor,
                        },
                    )],
                    id,
                )
                .map_err(|e| EditorError(e.to_string()))?;
                self.anchor = self.cursor;
            }
        } else if !execution.changes.is_empty()
            && execution.changes.iter().all(|change| change.detached)
        {
            self.mode = Mode::Normal;
            self.set_cursors(vec![original_cursor.min(self.text.len())], 0)?;
        } else if !execution.changes.is_empty() {
            let positions = cursor_marks
                .into_iter()
                .map(|mark| {
                    mark.and_then(|mark| self.text.mark_get(mark))
                        .unwrap_or_else(|| {
                            execution
                                .changes
                                .first()
                                .map_or(self.cursor, |change| change.range.start)
                        })
                })
                .collect();
            self.set_cursors(positions, primary)?;
            if self.selection_set.len() == 1 {
                if let Some(change) = execution
                    .changes
                    .iter()
                    .rev()
                    .find(|change| !change.detached)
                {
                    self.cursor = if change.contains_newline {
                        change.range.start
                    } else {
                        change.range.end
                    };
                    self.anchor = self.cursor;
                    self.sync_primary_to_set()?;
                }
            }
        }
        self.recording_change = false;
        self.command_keys.clear();
        Ok(EditorAction::Continue)
    }

    fn repeat(&mut self, count: Option<usize>) -> Result<EditorAction, EditorError> {
        if self.last_change.is_empty() {
            return Ok(EditorAction::Message("no previous change".into()));
        }
        let mut keys = self.last_change.clone();
        if let Some(count) = count {
            let mut start = 0;
            while matches!(keys.get(start),Some(Key::Char(c))if c.is_ascii_digit()) {
                start += 1;
            }
            keys.drain(..start);
            let mut operator = 0;
            if keys.first() == Some(&Key::Char('"')) && keys.len() > 1 {
                operator = 2;
            }
            let mut after_register = operator;
            while matches!(keys.get(after_register),Some(Key::Char(c))if c.is_ascii_digit()) {
                after_register += 1;
            }
            keys.drain(operator..after_register);
            if matches!(
                keys.get(operator),
                Some(Key::Char('d' | 'c' | 'y' | '<' | '>'))
            ) && !matches!(keys.get(operator + 1), Some(Key::Char('0')))
            {
                let mut end = operator + 1;
                while matches!(keys.get(end),Some(Key::Char(c))if c.is_ascii_digit()) {
                    end += 1;
                }
                keys.drain(operator + 1..end);
            }
            // The saved C action stores the resolved line-begin motion,
            // whose count is ignored. Replaying a textual count before d0
            // would instead turn its zero into another count digit.
            let line_begin = matches!(
                keys.get(operator),
                Some(Key::Char('d' | 'c' | 'y' | '<' | '>'))
            ) && keys.get(operator + 1) == Some(&Key::Char('0'));
            if !line_begin {
                let mut prefix: Vec<Key> = count.to_string().chars().map(Key::Char).collect();
                prefix.extend(keys);
                keys = prefix;
            }
            if !self.internal_scope {
                self.last_change = keys.clone();
            }
        }
        self.start_key_replay(KeyReplayKind::Dot, keys)
    }
    fn start_key_replay(
        &mut self,
        kind: KeyReplayKind,
        keys: Vec<Key>,
    ) -> Result<EditorAction, EditorError> {
        if kind == KeyReplayKind::Dot
            && self
                .key_replays
                .iter()
                .any(|frame| frame.kind == KeyReplayKind::Dot)
        {
            return Err(EditorError("recursive dot repeat".into()));
        }
        let frame = KeyReplay {
            kind,
            keys: keys.into(),
            outer: std::mem::take(&mut self.queued_keys),
            previous_replaying: self.replaying,
            pending_depth: self.callback_pending.len() + self.input_pending.len(),
        };
        if kind == KeyReplayKind::Dot {
            self.replaying = true;
        } else {
            self.macro_depth += 1;
        }
        self.key_replays.push(frame);
        if self.driving_replay {
            Ok(EditorAction::Continue)
        } else {
            self.drive_replay()
        }
    }
    fn drive_replay(&mut self) -> Result<EditorAction, EditorError> {
        self.driving_replay = true;
        let result = (|| {
            while let Some(frame) = self.key_replays.last_mut() {
                if frame.pending_depth < self.callback_pending.len() + self.input_pending.len() {
                    return Ok(EditorAction::Continue);
                }
                if let Some(key) = frame.keys.pop_front() {
                    let feeding = self.feeding_keys;
                    self.feeding_keys = true;
                    let result = self.handle_key(key);
                    self.feeding_keys = feeding;
                    let action = result?;
                    if action != EditorAction::Continue {
                        return Ok(action);
                    }
                } else {
                    let frame = self.key_replays.pop().expect("live replay frame");
                    self.queued_keys.extend(frame.outer);
                    if frame.kind == KeyReplayKind::Dot {
                        self.replaying = frame.previous_replaying;
                        self.recording_change = false;
                        self.command_keys.clear();
                    } else {
                        self.macro_depth -= 1;
                    }
                    if !self.queued_keys.is_empty() {
                        let action = self.next_queued()?;
                        if action != EditorAction::Continue {
                            return Ok(action);
                        }
                    }
                }
            }
            Ok(EditorAction::Continue)
        })();
        self.driving_replay = false;
        if result.is_err() {
            while let Some(frame) = self.key_replays.pop() {
                self.queued_keys.extend(frame.outer);
                if frame.kind == KeyReplayKind::Dot {
                    self.replaying = frame.previous_replaying;
                } else {
                    self.macro_depth = self.macro_depth.saturating_sub(1);
                }
            }
            self.cancel();
        }
        result
    }

    fn expand_mapping(&mut self, keys: Vec<Key>) -> Result<EditorAction, EditorError> {
        if self.mapping_depth == 0 {
            self.mapping_work = 0;
        }
        if self.mapping_depth >= 128 {
            return Err(EditorError("recursive mapping limit exceeded".into()));
        }
        self.mapping_work = self
            .mapping_work
            .checked_add(keys.len())
            .filter(|work| *work <= MAX_REPEAT_KEYS)
            .ok_or_else(|| EditorError("mapping expansion limit exceeded".into()))?;
        self.mapping_depth += 1;
        let result = (|| {
            let tail = std::mem::take(&mut self.queued_keys);
            self.enqueue_keys(keys)?;
            for (id, _) in &self.queued_keys {
                self.recorded_queued.insert(*id);
            }
            self.queued_keys.extend(tail);
            self.next_queued()
        })();
        self.mapping_depth -= 1;
        result
    }

    fn expand_literal_keys(&mut self, keys: Vec<Key>) -> Result<EditorAction, EditorError> {
        if keys.len() > MAX_REPEAT_KEYS
            || keys.len().saturating_add(self.queued_keys.len()) > MAX_REPEAT_KEYS
        {
            return Err(EditorError("mapping expansion limit exceeded".into()));
        }
        let tail = std::mem::take(&mut self.queued_keys);
        self.enqueue_keys(keys)?;
        for (id, _) in &self.queued_keys {
            self.literal_queued.insert(*id);
        }
        self.queued_keys.extend(tail);
        self.next_queued()
    }

    fn configure_mapping(
        &mut self,
        remove: bool,
        force: bool,
        args: &str,
    ) -> Result<(), EditorError> {
        let mut words = args.split_whitespace();
        let mut args: Vec<String> = words.by_ref().take(2).map(str::to_owned).collect();
        let right = words.collect::<Vec<_>>().join(" ");
        if !right.is_empty() {
            args.push(right);
        }
        self.configure_mapping_words(remove, force, &args, None)
    }
    fn configure_mapping_words(
        &mut self,
        remove: bool,
        force: bool,
        args: &[String],
        window: Option<u64>,
    ) -> Result<(), EditorError> {
        let mode = match args.first().map(String::as_str) {
            Some("normal") => Mode::Normal,
            Some("insert") => Mode::Insert,
            Some("visual") => Mode::Visual,
            Some("visual-line") => Mode::VisualLine,
            Some("operator-pending") => Mode::OperatorPending,
            Some("replace") => Mode::Replace,
            Some("command") => Mode::Command,
            _ => return Err(EditorError("mapping mode expected".into())),
        };
        let left = self.parse_keys(
            args.get(1)
                .ok_or_else(|| EditorError("mapping keys expected".into()))?,
        )?;
        if left.is_empty() {
            return Err(EditorError("empty mapping prefix".into()));
        }
        let index = self.mappings.iter().position(|mapping| {
            mapping.mode == mode && mapping.left == left && mapping.window == window
        });
        if remove {
            if let Some(index) = index {
                self.mappings.remove(index);
            }
            return Ok(());
        }
        let right = args
            .get(2)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| EditorError("mapping replacement expected".into()))?;
        let right = self.parse_keys(right)?;
        if index.is_some() && !force {
            return Err(EditorError("mapping already exists (use map!)".into()));
        }
        let mapping = Mapping {
            mode,
            left,
            right,
            callback: None,
            window,
        };
        if let Some(index) = index {
            self.mappings[index] = mapping;
        } else {
            self.mappings.push(mapping);
        }
        if let Some(window) = window {
            self.allocated_window_mapping_modes
                .insert((window, mode as u8));
        }
        Ok(())
    }
    /// Execute already-parsed native handlers without turning their argv back
    /// into a command string. The host retains strict/interactive error policy.
    pub(crate) fn execute_sam_host(
        &mut self,
        request: &sam::HostRequest,
    ) -> Result<EditorAction, EditorError> {
        use sam::HostCommand as H;
        match &request.command {
            H::Map | H::Unmap | H::MapWindow | H::UnmapWindow => {
                let window = if matches!(request.command, H::MapWindow | H::UnmapWindow) {
                    Some(
                        u64::try_from(request.window)
                            .map_err(|_| EditorError("window identifier out of range".into()))?,
                    )
                } else {
                    None
                };
                self.configure_mapping_words(
                    matches!(request.command, H::Unmap | H::UnmapWindow),
                    request.force,
                    &request.args,
                    window,
                )?;
            }
            H::LangMap => self.configure_langmap(&request.args)?,
            H::Earlier | H::Later => {
                self.mutate_text(|text| {
                    crate::history::execute_words(
                        text,
                        matches!(request.command, H::Earlier),
                        &request.args,
                    )
                })
                .map_err(|e| EditorError(e.to_string()))?;
            }
            _ => {
                return Err(EditorError(
                    "native host handler belongs to the workspace".into(),
                ));
            }
        }
        Ok(EditorAction::Continue)
    }

    pub fn notation_keys(mut input: &str) -> Result<Vec<Key>, EditorError> {
        let mut result = Vec::new();
        while !input.is_empty() {
            if input.starts_with('<') {
                if let Some(end) = input.find('>') {
                    let name = &input[1..end];
                    if let Some(action) = NativeAction::from_name(name) {
                        result.push(Key::Native(action));
                        input = &input[end + 1..];
                        continue;
                    }

                    let key = match name {
                        "Escape" | "Esc" => Some(Key::Escape),
                        "Enter" | "Return" | "CR" => Some(Key::Enter),
                        "Tab" => Some(Key::Tab),
                        "S-Tab" | "BackTab" => Some(Key::BackTab),
                        "Space" => Some(Key::Char(' ')),
                        "Backspace" | "BS" => Some(Key::Backspace),
                        "Delete" | "Del" => Some(Key::Delete),
                        "Left" => Some(Key::Left),
                        "Right" => Some(Key::Right),
                        "Up" => Some(Key::Up),
                        "Down" => Some(Key::Down),
                        "Home" => Some(Key::Home),
                        "End" => Some(Key::End),
                        "PageUp" => Some(Key::PageUp),
                        "PageDown" => Some(Key::PageDown),
                        "Insert" => Some(Key::Insert),
                        "S-Left" => Some(Key::ShiftLeft),
                        "S-Right" => Some(Key::ShiftRight),
                        "S-PageUp" => Some(Key::ShiftPageUp),
                        "S-PageDown" => Some(Key::ShiftPageDown),
                        "lt" => Some(Key::Char('<')),
                        "vis-prompt-show" => Some(Key::PromptShow),
                        "vis-search-forward" => Some(Key::PromptSearchForward),
                        "vis-search-backward" => Some(Key::PromptSearchBackward),
                        "vis-prompt-enter" => Some(Key::PromptSubmit),
                        "vis-prompt-hide" => Some(Key::PromptCancel),
                        "vis-prompt-expand" => Some(Key::PromptHistoryExpand),
                        "vis-selections-save" => Some(Key::SelectionsSave),
                        "vis-selections-restore" => Some(Key::SelectionsRestore),
                        "vis-nop" => {
                            input = &input[end + 1..];
                            continue;
                        }
                        _ => {
                            if let Some(control) = name.strip_prefix("C-") {
                                if control.chars().count() == 1 {
                                    control
                                        .chars()
                                        .next()
                                        .map(|c| Key::Ctrl(c.to_ascii_lowercase()))
                                } else {
                                    None
                                }
                            } else if let Some(control) = name.strip_prefix("M-C-") {
                                if control.chars().count() == 1 {
                                    control
                                        .chars()
                                        .next()
                                        .map(|c| Key::AltCtrl(c.to_ascii_lowercase()))
                                } else {
                                    None
                                }
                            } else if let Some(alt) = name.strip_prefix("M-") {
                                if alt.chars().count() == 1 {
                                    alt.chars().next().map(Key::Alt)
                                } else {
                                    None
                                }
                            } else if let Some(function) = name.strip_prefix('F') {
                                function
                                    .parse::<u8>()
                                    .ok()
                                    .filter(|n| (1..=24).contains(n))
                                    .map(Key::Function)
                            } else {
                                None
                            }
                        }
                    };
                    if let Some(key) = key {
                        result.push(key);
                        input = &input[end + 1..];
                        continue;
                    }
                    if name.starts_with("vis-") {
                        return Err(EditorError(format!("unsupported mapping action: {name}")));
                    }
                }
            }
            let c = input.chars().next().expect("nonempty mapping string");
            result.push(Key::Char(c));
            input = &input[c.len_utf8()..];
            if result.len() > MAX_REPEAT_KEYS {
                return Err(EditorError("mapping size limit exceeded".into()));
            }
        }
        Ok(result)
    }

    pub fn key_notation(key: Key) -> String {
        let name = match key {
            Key::Char(c) => return c.to_string(),
            Key::Ctrl(c) => return format!("<C-{c}>"),
            Key::Alt(c) => return format!("<M-{c}>"),
            Key::AltCtrl(c) => return format!("<M-C-{c}>"),
            Key::Function(n) => return format!("<F{n}>"),
            Key::Action(id) => return format!("<Action:{id}>"),
            Key::Native(action) => return format!("<{}>", action.name()),
            Key::SelectionsSave => "vis-selections-save",
            Key::SelectionsRestore => "vis-selections-restore",
            Key::PromptShow => "vis-prompt-show",
            Key::PromptSearchForward => "vis-search-forward",
            Key::PromptSearchBackward => "vis-search-backward",
            Key::PromptSubmit => "vis-prompt-enter",
            Key::PromptCancel => "vis-prompt-hide",
            Key::PromptHistoryExpand => "vis-prompt-expand",
            Key::Escape => "Escape",
            Key::Enter => "Enter",
            Key::Backspace => "Backspace",
            Key::Delete => "Delete",
            Key::Left => "Left",
            Key::Right => "Right",
            Key::Up => "Up",
            Key::Down => "Down",
            Key::Home => "Home",
            Key::End => "End",
            Key::Tab => "Tab",
            Key::BackTab => "S-Tab",
            Key::PageUp => "PageUp",
            Key::PageDown => "PageDown",
            Key::Insert => "Insert",
            Key::ShiftLeft => "S-Left",
            Key::ShiftRight => "S-Right",
            Key::ShiftPageUp => "S-PageUp",
            Key::ShiftPageDown => "S-PageDown",
        };
        format!("<{name}>")
    }

    fn play_macro(&mut self, name: char, count: usize) -> Result<EditorAction, EditorError> {
        if self.macro_depth >= 16 {
            return Err(EditorError("macro nesting limit exceeded".into()));
        }
        let name = name.to_ascii_lowercase();
        if self
            .macro_record
            .is_some_and(|active| active.to_ascii_lowercase() == name)
        {
            return Ok(EditorAction::Message(
                "cannot replay the recording register".into(),
            ));
        }
        if name == '/' {
            for _ in 0..count {
                self.search_motion(false)?;
            }
            return Ok(EditorAction::Continue);
        }
        if name == ':' {
            let command = self
                .register(':')
                .map(|r| String::from_utf8_lossy(&r.bytes).into_owned())
                .unwrap_or_default();
            let mut action = EditorAction::Continue;
            for _ in 0..count {
                action = self.execute_command(&command)?;
                if action != EditorAction::Continue {
                    break;
                }
            }
            return Ok(action);
        }
        let bytes = self
            .register(name)
            .map(|register| register.bytes.clone())
            .unwrap_or_default();
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| EditorError("macro register is not UTF-8".into()))?;
        let keys = self.parse_keys(source)?;
        let work = keys
            .len()
            .checked_mul(count)
            .ok_or_else(|| EditorError("macro work limit exceeded".into()))?;
        if self.macro_depth == 0 {
            self.macro_work = 0;
        }
        self.macro_work = self
            .macro_work
            .checked_add(work)
            .filter(|work| *work <= MAX_REPEAT_KEYS)
            .ok_or_else(|| EditorError("macro work limit exceeded".into()))?;
        let keys = keys.iter().copied().cycle().take(work).collect();
        self.start_key_replay(KeyReplayKind::Macro, keys)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn editor(bytes: &[u8]) -> Editor {
        Editor::new(Text::from_bytes(bytes))
    }
    fn keys(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            editor.handle_key(Key::Char(c)).unwrap();
        }
    }
    fn escape(editor: &mut Editor) {
        editor.handle_key(Key::Escape).unwrap();
    }

    #[test]
    fn earlier_and_later_report_the_selected_state_time() {
        let mut e = editor(b"old\n");
        e.text.insert(4, b"new\n").unwrap();
        e.text.snapshot();
        assert_eq!(e.text.to_vec(), b"old\nnew\n");

        let action = e.execute_command("earlier").unwrap();
        assert_eq!(e.text.to_vec(), b"old\n");
        assert_eq!(
            action,
            EditorAction::Message(format!(
                "State from {}",
                crate::time::local_hm(e.text.state())
            ))
        );

        let action = e.execute_command("later").unwrap();
        assert_eq!(e.text.to_vec(), b"old\nnew\n");
        assert_eq!(
            action,
            EditorAction::Message(format!(
                "State from {}",
                crate::time::local_hm(e.text.state())
            ))
        );
    }

    #[test]
    fn missing_find_repeat_does_not_cancel_pending_operators() {
        let mut e = editor(b"one two three\n");
        keys(&mut e, "d;w");
        assert_eq!(e.text.to_vec(), b"two three\n");

        let mut e = editor(b"one two\n");
        keys(&mut e, "c,wX");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"X two\n");
    }

    #[test]
    fn operator_change_at_eof_keeps_native_linewise_newlines() {
        let mut e = editor(b"one two three\nfour five\nsix\n");
        keys(&mut e, "2c3wX");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"X\n\n");
    }

    #[test]
    fn native_linewise_motion_ranges_and_missing_objects_match_c() {
        // Counted G is the non-inclusive line motion: the target line stays.
        let mut e = editor(b"one\ntwo\nthree\nfour\n");
        keys(&mut e, "d3G");
        assert_eq!(e.text.to_vec(), b"three\nfour\n");
        // G without a count is FILE_END and is linewise inclusive.
        let mut e = editor(b"one\ntwo\nthree\n");
        keys(&mut e, "dG");
        assert_eq!(e.text.to_vec(), b"");
        // Paragraph motions are linewise, so an empty change inserts one
        // newline before the entered text.
        let mut e = editor(b"one two three\nfour five\nsix\n");
        keys(&mut e, "c{X");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"X\none two three\nfour five\nsix\n");
        // A missing text object leaves an empty range for the operator.
        let mut e = editor(b"one two\n");
        keys(&mut e, "ci(X");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"Xone two\n");
        // % without a bracket under the cursor scans forward like native.
        let mut e = editor(b"alpha.beta, gamma_delta (x) [y] {z}\n");
        keys(&mut e, "d%");
        assert_eq!(e.text.to_vec(), b"x) [y] {z}\n");
    }

    #[test]
    fn visual_line_entry_consumes_count_and_escape_resets_charwise_count() {
        let mut e = editor(b"one two three\nfour five\nsix\n");
        keys(&mut e, "2Vj");
        assert_eq!(e.mode, Mode::VisualLine);
        assert_eq!(e.cursor, 23);
        assert_eq!(e.selection_ranges(), vec![0..24]);

        let mut e = editor(b"one two\n");
        keys(&mut e, "2v");
        e.handle_key(Key::Escape).unwrap();
        assert_eq!(e.mode, Mode::Visual);
        e.handle_key(Key::Escape).unwrap();
        assert_eq!(e.mode, Mode::Normal);

        let mut e = editor(b"one two three\nfour five\nsix\n");
        keys(&mut e, "2VjP");
        assert_eq!(e.text.to_vec(), b"six\n");
        keys(&mut e, ".");
        assert_eq!(e.text.to_vec(), b"six\n");
    }

    #[test]
    fn explicit_named_yank_does_not_replace_unnamed_put_register() {
        let mut e = editor(b"one two three\nfour five\nsix\n");
        keys(&mut e, "\"ayy");
        assert_eq!(e.register('a').unwrap().bytes, b"one two three\n");
        assert!(
            e.register('"')
                .is_none_or(|register| register.bytes.is_empty())
        );

        keys(&mut e, "jVjP.");
        assert_eq!(e.text.to_vec(), b"one two three\n");
    }
    #[test]
    fn insert_unicode_binary_and_grouped_undo() {
        let mut e = editor(b"\xffworld\n");
        keys(&mut e, "i♪");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"\xe2\x99\xaa\xffworld\n");
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"\xffworld\n");
        e.handle_key(Key::Ctrl('r')).unwrap();
        assert_eq!(e.text.to_vec(), b"\xe2\x99\xaa\xffworld\n");
    }
    #[test]
    fn counted_delete_motion_and_register_put() {
        let mut e = editor(b"one two three four five six seven\n");
        keys(&mut e, "2d3w");
        // vis accumulates one action count through the pending operator:
        // 2d3w means twenty-three words, rather than multiplying 2 by 3.
        assert_eq!(e.text.to_vec(), b"");
        assert_eq!(
            e.register('"').unwrap().bytes,
            b"one two three four five six seven\n"
        );
        keys(&mut e, "P");
        assert_eq!(e.text.to_vec(), b"one two three four five six seven\n");
    }
    #[test]
    fn linewise_change_and_yank_append() {
        let mut e = editor(b"one\ntwo\nthree\n");
        keys(&mut e, "\"ayyj\"Ayy");
        assert_eq!(e.register('a').unwrap().bytes, b"one\ntwo\n");
        keys(&mut e, "ccchanged");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"one\nchanged\nthree\n");
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"one\ntwo\nthree\n");
    }
    #[test]
    fn change_word_repeat_and_counted_insert() {
        let mut e = editor(b"one two three\n");
        keys(&mut e, "cwfoo");
        escape(&mut e);
        keys(&mut e, "w.");
        assert_eq!(e.text.to_vec(), b"foo foo three\n");
        let mut e = editor(b"abc\n");
        keys(&mut e, "4i#");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"####abc\n");
        keys(&mut e, "$2.");
        assert_eq!(e.text.to_vec(), b"####abc##\n");
    }
    #[test]
    fn text_objects_visual_indent_and_blackhole() {
        let mut e = editor(b"(one two)\nthree\n");
        keys(&mut e, "di(");
        assert_eq!(e.text.to_vec(), b"()\nthree\n");
        keys(&mut e, "\"_dd");
        assert_eq!(e.text.to_vec(), b"three\n");
        assert_eq!(e.register('"').unwrap().bytes, b"one two");
        keys(&mut e, "V>");
        assert_eq!(e.text.to_vec(), b"\tthree\n");
        keys(&mut e, "<<");
        assert_eq!(e.text.to_vec(), b"three\n");
    }
    #[test]
    fn prompt_commands_are_host_requests_and_search_wraps() {
        let mut e = editor(b"one two one\n");
        keys(&mut e, "/one");
        e.handle_key(Key::Enter).unwrap();
        assert_eq!(e.cursor, 8);
        keys(&mut e, "n");
        assert_eq!(e.cursor, 0);
        keys(&mut e, ":wq!");
        assert_eq!(
            e.handle_key(Key::Enter).unwrap(),
            EditorAction::Save {
                quit: true,
                force: true,
                path: None,
                ranges: None,
                visual: false,
            }
        );
        e.execute_command(",x/one/ c/ONE/").unwrap();
        assert_eq!(e.text.to_vec(), b"ONE two ONE\n");
    }
    #[test]
    fn join_and_invalid_keys_preserve_state() {
        let mut e = editor(b"one\n  two\nthree\n");
        keys(&mut e, "3J");
        assert_eq!(e.text.to_vec(), b"one two three\n");
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"one\n  two\nthree\n");
        assert!(e.handle_key(Key::Char('!')).is_err());
        assert_eq!(e.mode, Mode::Normal);
    }

    #[test]
    fn vis_insertion_endpoint_and_vertical_display_column() {
        let mut e = editor(b"one two\nthree\n");
        keys(&mut e, "wix");
        escape(&mut e);
        assert_eq!(e.cursor, 5);
        keys(&mut e, "ji!");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"one xtwo\nthree!\n");
        assert_eq!(e.cursor, 15);
    }

    #[test]
    fn counted_open_lines_and_visual_replacement_cancellation() {
        let mut e = editor(b"old\n");
        keys(&mut e, "2Obefore");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"before\nbefore\nold\n");
        let mut e = editor(b"one two three\n");
        keys(&mut e, "v2er");
        escape(&mut e);
        assert_eq!(e.mode, Mode::Visual);
        assert_eq!(e.text.to_vec(), b"one two three\n");
        keys(&mut e, "r=");
        assert_eq!(e.text.to_vec(), b"======= three\n");
    }

    #[test]
    fn bracketed_paste_is_literal_and_undoable() {
        let mut e = editor(b"text\n");
        let paste = b"\x1b:q!\n\0\xff";
        assert!(e.paste(paste).is_err());
        assert_eq!(e.text.to_vec(), b"text\n");
        keys(&mut e, "i");
        e.paste(paste).unwrap();
        escape(&mut e);
        assert_eq!(e.text.bytes(0..paste.len()).unwrap(), paste);
        assert_eq!(e.mode, Mode::Normal);
        assert!(matches!(
            e.handle_key(Key::Char('.')).unwrap(),
            EditorAction::Message(_)
        ));
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"text\n");
    }

    #[test]
    fn multiple_line_cursors_edit_and_undo_together() {
        let mut e = editor(b"one\ntwo\nthree\n");
        keys(&mut e, "vGI");
        assert_eq!(
            e.selections().iter().map(|s| s.cursor).collect::<Vec<_>>(),
            vec![0, 4, 8]
        );
        assert_eq!(e.primary_index(), 2);
        keys(&mut e, "i#");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"#one\n#two\n#three\n");
        assert_eq!(
            e.selections().iter().map(|s| s.cursor).collect::<Vec<_>>(),
            vec![1, 6, 11]
        );
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"one\ntwo\nthree\n");
        assert_eq!(
            e.selections().iter().map(|s| s.cursor).collect::<Vec<_>>(),
            vec![0, 4, 8]
        );
    }

    #[test]
    fn repeated_till_and_named_register_slots_use_all_cursors() {
        let mut e = editor(b"1 : first\n2 : second\n");
        keys(&mut e, "vGA$dT ");
        keys(&mut e, ".");
        assert_eq!(e.text.to_vec(), b"1 \n2 \n");
        let mut e = editor(b"one A\ntwo B\n");
        keys(&mut e, "vGI\"ayw");
        keys(&mut e, "$\"ap");
        assert_eq!(e.text.to_vec(), b"one Aone \ntwo Btwo \n");
    }

    #[test]
    fn structural_commands_retain_every_result_selection() {
        let mut e = editor(b"one two one\n");
        e.execute_command(",x/one/").unwrap();
        assert_eq!(e.selection_ranges(), vec![0..3, 8..11]);
        assert_eq!(e.mode, Mode::Visual);
        keys(&mut e, "cX");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"X two X\n");
        assert_eq!(e.selections().len(), 2);
    }

    #[test]
    fn macro_registers_are_live_text_and_last_recorded_macro_replays() {
        let mut e = editor(b"text\n");
        keys(&mut e, "qaiX");
        escape(&mut e);
        keys(&mut e, "q");
        assert_eq!(e.register('a').unwrap().bytes, b"iX<Escape>");
        keys(&mut e, "@@");
        assert_eq!(e.text.to_vec(), b"XXtext\n");
        e.selected_register = Some('a');
        e.store_register(b"iY<Escape>".to_vec(), false, false);
        e.selected_register = None;
        keys(&mut e, "@a");
        assert_eq!(e.text.to_vec(), b"XXYtext\n");
        e.selected_register = Some('b');
        e.store_register(b"@b".to_vec(), false, false);
        e.selected_register = None;
        keys(&mut e, "@");
        assert!(e.handle_key(Key::Char('b')).is_err());
        assert_eq!(e.macro_depth, 0);
        assert_eq!(e.text.to_vec(), b"XXYtext\n");
    }

    #[test]
    fn mapping_prefixes_flush_and_literal_input_stays_literal() {
        let mut e = editor(b"text\n");
        e.execute_command("map! insert jj ><").unwrap();
        keys(&mut e, "ijj");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"><text\n");
        let mut e = editor(b"text\n");
        e.execute_command("map! insert jj <vis-nop>").unwrap();
        keys(&mut e, "ij");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"jtext\n");
        let mut e = editor(b"");
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('k')).unwrap();
        keys(&mut e, "l*");
        e.handle_key(Key::Ctrl('v')).unwrap();
        keys(&mut e, "u2200");
        e.expand_tab = true;
        e.handle_key(Key::Ctrl('v')).unwrap();
        e.handle_key(Key::Tab).unwrap();
        e.handle_key(Key::Ctrl('v')).unwrap();
        e.handle_key(Key::Escape).unwrap();
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"\xce\xbb\xe2\x88\x80\t\x1b");
    }

    #[test]
    fn repeat_count_replaces_inner_count_and_preserves_zero_motion() {
        let mut e = editor(b"one two\n");
        keys(&mut e, "d0$99.");
        assert_eq!(e.text.to_vec(), b"\n");
        let mut e = editor(b"void a(void);\nvoid b(void);\nvoid c(void);\n");
        e.search = "void".into();
        e.session.set_previous_regex("void").unwrap();
        keys(&mut e, "cgnstatic void");
        escape(&mut e);
        keys(&mut e, "2..");
        assert_eq!(
            e.text.to_vec(),
            b"static void a(void);\nstatic void b(void);\nstatic void c(void);\n"
        );
    }

    #[test]
    fn prompt_history_and_two_stage_escape_keep_document_state() {
        let mut e = editor(b"one\ntwo\nthree\n");
        keys(&mut e, ":1");
        e.handle_key(Key::Enter).unwrap();
        keys(&mut e, ":2");
        e.handle_key(Key::Enter).unwrap();
        keys(&mut e, ":");
        e.handle_key(Key::Up).unwrap();
        e.handle_key(Key::Up).unwrap();
        e.handle_key(Key::Enter).unwrap();
        assert_eq!(e.cursor, 0);
        keys(&mut e, ":unfinished");
        escape(&mut e);
        assert_eq!(e.mode, Mode::Command);
        escape(&mut e);
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.text.to_vec(), b"one\ntwo\nthree\n");
    }

    #[test]
    #[allow(clippy::single_range_in_vec_init)] // These containers hold byte ranges.
    fn writes_resolve_raw_addresses_and_preserve_visual_view() {
        let mut e = editor(b"abcdef\n");
        assert_eq!(
            e.execute_command("#1,#3w! 'other file'").unwrap(),
            EditorAction::Save {
                quit: false,
                force: true,
                path: Some("'other file'".into()),
                ranges: Some(vec![1..3]),
                visual: false
            }
        );
        keys(&mut e, "v2l");
        let action = e.execute_command("w copy").unwrap();
        assert!(
            matches!(action,EditorAction::Save{ranges:Some(ref ranges),visual:true,..}if ranges.as_slice()==[0..3])
        );
        assert_eq!(e.mode, Mode::Visual);
        assert_eq!(e.selection_ranges(), vec![0..3]);
        assert_eq!(e.text.to_vec(), b"abcdef\n");
    }

    #[test]
    fn buffer_state_moves_owned_marks_without_leaking_between_documents() {
        let mut e = editor(b"first\n");
        keys(&mut e, "v2lm");
        let mut saved = BufferState::default();
        e.swap_buffer_state(&mut saved);
        let original = std::mem::replace(&mut e.text, Text::from_bytes(b"second\n"));
        e.restore_view(
            vec![Selection {
                anchor: 0,
                cursor: 0,
            }],
            0,
            Mode::Normal,
        )
        .unwrap();
        assert!(e.saved_ranges('\'').is_empty());
        e.text = original;
        e.swap_buffer_state(&mut saved);
        e.restore_view(
            vec![Selection {
                anchor: 0,
                cursor: 0,
            }],
            0,
            Mode::Normal,
        )
        .unwrap();
        assert_eq!(e.saved_ranges('\''), vec![0..3]);
    }

    #[test]
    fn control_line_endings_confirm_prompts_without_changing_selection_controls() {
        for confirm in [Key::Ctrl('j'), Key::Ctrl('m')] {
            let mut e = editor(b"one\ntwo\n");
            keys(&mut e, ":q");
            assert_eq!(
                e.handle_key(confirm).unwrap(),
                EditorAction::Quit { force: false }
            );
            assert_eq!(e.mode, Mode::Normal);
            assert_eq!(e.text.to_vec(), b"one\ntwo\n");
        }
        let mut e = editor(b"one\ntwo\n");
        e.handle_key(Key::Ctrl('j')).unwrap();
        assert_eq!(e.selections().len(), 2);
        assert_eq!(e.cursor, 4);
        keys(&mut e, "v");
        e.handle_key(Key::Ctrl('j')).unwrap();
        assert_eq!(e.mode, Mode::Visual);
        assert_eq!(e.primary_index(), 0);
        assert_eq!(e.text.to_vec(), b"one\ntwo\n");
        let mut e = editor(b"one\ntwo\n");
        e.handle_key(Key::Ctrl('m')).unwrap();
        assert_eq!(e.cursor, 4);
        assert_eq!(e.selections().len(), 1);
    }

    #[test]
    fn insert_control_j_is_literal_lf_while_control_m_is_smart_enter() {
        for mode_key in ['i', 'R'] {
            let mut e = editor(b"\tword\n");
            e.cursor = 1;
            e.auto_indent = true;
            keys(&mut e, &mode_key.to_string());
            e.handle_key(Key::Ctrl('j')).unwrap();
            assert_eq!(e.text.to_vec(), b"\t\nword\n");
            assert_eq!(e.cursor, 2);
            let mut e = editor(b"\tword\n");
            e.cursor = 1;
            e.auto_indent = true;
            keys(&mut e, &mode_key.to_string());
            e.handle_key(Key::Ctrl('m')).unwrap();
            assert_eq!(e.text.to_vec(), b"\t\n\tword\n");
            assert_eq!(e.cursor, 3);
        }
        let mut e = editor(b"");
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('j')).unwrap();
        assert_eq!(e.text.to_vec(), b"\n");
        let mut e = editor(b"");
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('m')).unwrap();
        assert_eq!(e.text.to_vec(), b"\n\n");
    }

    #[test]
    fn pending_verbatim_keeps_lf_and_cr_distinct_and_never_trims_literal_input() {
        for mode_key in ['i', 'R'] {
            for (literal, bytes) in [
                (Key::Ctrl('j'), &b"\n"[..]),
                (Key::Ctrl('m'), &b"\r"[..]),
                (Key::Enter, &b"\r"[..]),
            ] {
                let mut e = editor(b"word\n");
                keys(&mut e, &mode_key.to_string());
                e.handle_key(Key::Ctrl('v')).unwrap();
                e.handle_key(literal).unwrap();
                assert_eq!(e.text.bytes(0..bytes.len()).unwrap(), bytes);
                assert_eq!(e.text.bytes(bytes.len()..e.text.len()).unwrap(), b"word\n");
            }
        }
        let mut e = editor(b"\t\n");
        e.cursor = 1;
        e.auto_indent = true;
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('v')).unwrap();
        e.handle_key(Key::Enter).unwrap();
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"\t\r\n");
        let mut e = editor(b"\t\n");
        e.cursor = 1;
        e.auto_indent = true;
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('m')).unwrap();
        escape(&mut e);
        assert_eq!(e.text.to_vec(), b"\n\n");
        let mut e = editor(b"word\n");
        keys(&mut e, "i");
        e.handle_key(Key::Ctrl('k')).unwrap();
        assert!(e.handle_key(Key::Ctrl('j')).is_err());
        assert_eq!(e.text.to_vec(), b"word\n");
    }
    fn callback(action: EditorAction) -> CallbackRequest {
        match action {
            EditorAction::Callback(request) => request,
            _ => panic!("callback request expected"),
        }
    }
    #[test]
    fn selection_handles_survive_motion_edit_and_window_restore() {
        let mut e = editor(b"abc\ndef\n");
        let id = e.selection_ids()[0];
        keys(&mut e, "liX");
        assert_eq!(e.selection_ids(), &[id]);
        escape(&mut e);
        let state = e.view_state();
        e.restore_view_state(state, id, Mode::Normal).unwrap();
        assert_eq!(e.selection_ids(), &[id]);
        e.set_selection_anchored(id, true).unwrap();
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.selection_anchored(id), Some(true));
        let other = e
            .create_selection(
                Selection {
                    anchor: 5,
                    cursor: 5,
                },
                false,
            )
            .unwrap();
        e.remove_selection(id).unwrap();
        assert!(e.selection_by_id(id).is_none());
        assert!(e.selection_by_id(other).is_some());
    }
    #[test]
    fn plugin_text_edits_keep_deleted_selection_marks_invalid_until_undo() {
        let mut e = editor(b"abcdef");
        let id = e.selection_ids()[0];
        e.update_selection(
            id,
            Selection {
                anchor: 1,
                cursor: 2,
            },
        )
        .unwrap();
        e.text.snapshot();
        e.mutate_text(|t| t.delete(2..3)).unwrap();
        assert_eq!(e.selection_position(id), None);
        assert_eq!(e.selection_range(id), None);
        e.mutate_text(|t| t.snapshot());
        assert_eq!(e.selection_position(id), None);
        e.mutate_text(|t| t.insert(t.len(), b"!")).unwrap();
        assert_eq!(e.selection_position(id), None);
        e.mutate_text(|t| t.snapshot());
        e.mutate_text(|t| t.undo());
        assert_eq!(e.selection_position(id), None);
        e.mutate_text(|t| t.undo());
        assert_eq!(e.selection_position(id), Some(2));
        assert_eq!(e.selection_range(id), Some(1..3));
    }
    #[test]
    fn callback_mapping_wait_consumes_symbolic_tail_and_reentrant_feed_runs_first() {
        let mut e = editor(b"abc");
        e.register_callback(CallbackType::Mapping, 7).unwrap();
        e.map_callback(Mode::Normal, "Q", 7, None).unwrap();
        let request = callback(e.feed_keys(Editor::notation_keys("Qll").unwrap()).unwrap());
        assert_eq!(request.kind, CallbackKind::Mapping { keys: "ll".into() });
        assert_eq!(
            e.feed_keys(Editor::notation_keys("iX<Escape>").unwrap())
                .unwrap(),
            EditorAction::Continue
        );
        assert_eq!(e.text.to_vec(), b"Xabc");
        assert_eq!(e.cursor, 1);
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.cursor, 2);
        let request = callback(e.handle_key(Key::Char('Q')).unwrap());
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(-1)))
            .unwrap();
        let request = callback(e.handle_key(Key::Char('l')).unwrap());
        assert_eq!(request.kind, CallbackKind::Mapping { keys: "l".into() });
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.cursor, 2);
        assert!(
            e.resume_callback(request.token, CallbackResponse::Mapping(None))
                .is_err()
        );
    }
    #[test]
    fn callback_motion_and_textobject_complete_native_pending_operators() {
        let mut e = editor(b"one two");
        e.register_callback(CallbackType::Motion, 1).unwrap();
        e.register_callback(CallbackType::TextObject, 2).unwrap();
        keys(&mut e, "d");
        let request = callback(
            e.invoke_callback(CallbackKind::Motion { pos: e.cursor }, 1)
                .unwrap(),
        );
        e.resume_callback(request.token, CallbackResponse::Position(Some(4)))
            .unwrap();
        assert_eq!(e.text.to_vec(), b"two");
        keys(&mut e, "d");
        let request = callback(
            e.invoke_callback(CallbackKind::TextObject { pos: 0 }, 2)
                .unwrap(),
        );
        e.resume_callback(request.token, CallbackResponse::Range(Some(0..2)))
            .unwrap();
        assert_eq!(e.text.to_vec(), b"o");
    }
    #[test]
    fn callback_operator_can_mutate_text_and_removed_selection_cannot_be_reused() {
        let mut e = editor(b"abc");
        e.register_callback(CallbackType::Operator, 1).unwrap();
        assert_eq!(
            e.invoke_callback(
                CallbackKind::Operator {
                    range: 0..0,
                    pos: 0
                },
                1
            )
            .unwrap(),
            EditorAction::Continue
        );
        let request = callback(e.handle_key(Key::Char('l')).unwrap());
        assert_eq!(
            request.kind,
            CallbackKind::Operator {
                range: 0..1,
                pos: 0
            }
        );
        e.mutate_text(|t| t.delete(0..1)).unwrap();
        e.resume_callback(request.token, CallbackResponse::Position(Some(0)))
            .unwrap();
        assert_eq!(e.text.to_vec(), b"bc");
        assert_eq!(e.mode, Mode::Normal);
        e.register_callback(CallbackType::Motion, 2).unwrap();
        let old = e.selection_ids()[0];
        let request = callback(
            e.invoke_callback(CallbackKind::Motion { pos: 0 }, 2)
                .unwrap(),
        );
        let new = e
            .create_selection(
                Selection {
                    anchor: 1,
                    cursor: 1,
                },
                true,
            )
            .unwrap();
        e.remove_selection(old).unwrap();
        e.resume_callback(request.token, CallbackResponse::Position(Some(0)))
            .unwrap();
        assert_eq!(e.selection_ids(), &[new]);
        assert_eq!(e.cursor, 1);
    }
    #[test]
    fn callbacks_visit_all_selections_and_repeat_motion_counts() {
        let mut e = editor(b"abc\ndef\n");
        let first = e.selection_ids()[0];
        let second = e
            .create_selection(
                Selection {
                    anchor: 4,
                    cursor: 4,
                },
                false,
            )
            .unwrap();
        e.register_callback(CallbackType::Motion, 1).unwrap();
        e.set_count(Some(2)).unwrap();
        let mut action = e
            .invoke_callback(CallbackKind::Motion { pos: 0 }, 1)
            .unwrap();
        let mut visited = Vec::new();
        loop {
            let request = match action {
                EditorAction::Callback(request) => request,
                EditorAction::Continue => break,
                _ => panic!("unexpected action"),
            };
            let CallbackKind::Motion { pos } = request.kind else {
                panic!("motion expected")
            };
            visited.push(pos);
            action = e
                .resume_callback(request.token, CallbackResponse::Position(Some(pos + 1)))
                .unwrap();
        }
        assert_eq!(visited, vec![0, 1, 4, 5]);
        assert_eq!(e.selection_by_id(first).unwrap().cursor, 2);
        assert_eq!(e.selection_by_id(second).unwrap().cursor, 6);
        assert_eq!(e.selection_ids()[e.primary_index()], first);
        e.set_count(Some(99)).unwrap();
        let request = callback(
            e.invoke_callback(CallbackKind::Motion { pos: 2 }, 1)
                .unwrap(),
        );
        let next = e
            .resume_callback(request.token, CallbackResponse::Position(Some(2)))
            .unwrap();
        let request = callback(next);
        assert_eq!(request.kind, CallbackKind::Motion { pos: 6 });
        assert_eq!(
            e.resume_callback(request.token, CallbackResponse::Position(Some(6)))
                .unwrap(),
            EditorAction::Continue
        );
    }
    #[test]
    fn callback_operators_follow_ranges_shifted_by_previous_callback_edits() {
        let mut e = editor(b"ab cd");
        let first = e.selection_ids()[0];
        let second = e
            .create_selection(
                Selection {
                    anchor: 3,
                    cursor: 4,
                },
                false,
            )
            .unwrap();
        e.update_selection(
            first,
            Selection {
                anchor: 0,
                cursor: 1,
            },
        )
        .unwrap();
        e.mode = Mode::Visual;
        e.register_callback(CallbackType::Operator, 9).unwrap();
        let request = callback(
            e.invoke_callback(
                CallbackKind::Operator {
                    range: 0..0,
                    pos: 0,
                },
                9,
            )
            .unwrap(),
        );
        assert_eq!(
            request.kind,
            CallbackKind::Operator {
                range: 0..2,
                pos: 1
            }
        );
        e.mutate_text(|t| t.delete(0..2)).unwrap();
        let request = callback(
            e.resume_callback(request.token, CallbackResponse::Position(Some(0)))
                .unwrap(),
        );
        assert_eq!(
            request.kind,
            CallbackKind::Operator {
                range: 1..3,
                pos: 2
            }
        );
        e.mutate_text(|t| t.delete(1..3)).unwrap();
        e.resume_callback(request.token, CallbackResponse::Position(Some(1)))
            .unwrap();
        assert_eq!(e.text.to_vec(), b" ");
        assert_eq!(e.selection_by_id(first).unwrap().cursor, 0);
        assert_eq!(e.selection_by_id(second).unwrap().cursor, 1);
    }
    #[test]
    fn invalid_selection_marks_survive_view_restore_and_are_document_owned() {
        let mut e = editor(b"abcd");
        let id = e.selection_ids()[0];
        e.update_selection(
            id,
            Selection {
                anchor: 1,
                cursor: 2,
            },
        )
        .unwrap();
        e.mutate_text(|t| t.delete(1..2)).unwrap();
        assert_eq!(e.selection_position(id), Some(1));
        assert_eq!(e.selection_range(id), None);
        let marks = e.selection_marks(id).unwrap();
        let state = e.view_state();
        e.restore_view_state(state, id, Mode::Normal).unwrap();
        e.restore_selection_marks(vec![(id, marks.clone())])
            .unwrap();
        assert_eq!(e.selection_range(id), None);
        assert_eq!(e.selection_position(id), Some(1));
        let other = Text::from_bytes(b"abcd");
        assert!(!marks.belongs_to(&other));
        e.text = other;
        assert!(e.restore_selection_marks(vec![(id, marks)]).is_err());
    }
    #[test]
    fn sam_result_identities_distinguish_attached_and_detached_results() {
        let mut e = editor(b"ab cd");
        let original = e.selection_ids()[0];
        e.execute_command("#0,#2p").unwrap();
        assert_eq!(e.selection_ids(), &[original]);
        e.execute_command("{ #0,#2p #3,#5p }").unwrap();
        assert_eq!(e.selections().len(), 2);
        assert!(!e.selection_ids().contains(&original));
        let ids = e.selection_ids().to_vec();
        e.execute_command("p").unwrap();
        assert_eq!(e.selection_ids(), ids);
    }
    #[test]
    fn raw_replace_counts_characters_and_stops_at_line_end() {
        let mut e = editor("a♪bc\ndef".as_bytes());
        e.insert_raw("界X".as_bytes(), true).unwrap();
        assert_eq!(e.text.to_vec(), "界Xbc\ndef".as_bytes());
        assert_eq!(e.mode, Mode::Normal);
        e.update_selection(
            e.selection_ids()[0],
            Selection {
                anchor: 4,
                cursor: 4,
            },
        )
        .unwrap();
        e.insert_raw(b"12345", true).unwrap();
        assert_eq!(e.text.to_vec(), "界X12345\ndef".as_bytes());
    }
    #[test]
    fn named_actions_and_builtin_selection_actions_parse_as_symbolic_keys() {
        let mut e = editor(b"abcd");
        e.register_callback(CallbackType::Mapping, 44).unwrap();
        e.register_action("custom-action", 44).unwrap();
        let parsed = e
            .parse_keys("<custom-action><vis-selections-save>ll<vis-selections-restore>")
            .unwrap();
        let request = callback(e.feed_keys(parsed).unwrap());
        assert_eq!(
            request.kind,
            CallbackKind::Mapping {
                keys: "<vis-selections-save>ll<vis-selections-restore>".into()
            }
        );
        e.resume_callback(request.token, CallbackResponse::Mapping(None))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.cursor, 0);
    }
    #[test]
    fn mapping_resume_uses_callback_chosen_document_and_mode() {
        let mut e = editor(b"old");
        e.set_window_id(Some(1));
        e.register_callback(CallbackType::Mapping, 2).unwrap();
        e.map_callback(Mode::Normal, "Q", 2, None).unwrap();
        let request = callback(e.feed_keys(e.parse_keys("QiX<Escape>").unwrap()).unwrap());
        assert_eq!(request.window, Some(1));
        e.text = Text::from_bytes(b"new");
        e.restore_view(
            vec![Selection {
                anchor: 0,
                cursor: 0,
            }],
            0,
            Mode::Normal,
        )
        .unwrap();
        e.set_window_id(Some(2));
        e.resume_callback(request.token, CallbackResponse::Mapping(None))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.text.to_vec(), b"Xnew");
    }
    #[test]
    fn alias_callback_retains_rhs_tail_and_window_mapping_overrides_global() {
        let mut e = editor(b"abc");
        e.register_callback(CallbackType::Mapping, 5).unwrap();
        e.register_action("custom", 5).unwrap();
        e.map_alias(Mode::Normal, "Q", "<custom>ll", None).unwrap();
        let request = callback(e.handle_key(Key::Char('Q')).unwrap());
        assert_eq!(request.kind, CallbackKind::Mapping { keys: "ll".into() });
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.cursor, 1);
        e.set_window_id(Some(1));
        e.map_alias(Mode::Normal, "Z", "h", None).unwrap();
        e.map_alias(Mode::Normal, "Z", "l", Some(1)).unwrap();
        e.handle_key(Key::Char('Z')).unwrap();
        assert_eq!(e.cursor, 2);
    }
    #[test]
    fn feedkeys_bypasses_macro_recording_but_native_mapping_arguments_record_once() {
        let mut e = editor(b"abc");
        keys(&mut e, "qa");
        e.feed_keys(e.parse_keys("iX<Escape>").unwrap()).unwrap();
        assert!(e.register('a').unwrap().bytes.is_empty());
        e.register_callback(CallbackType::Mapping, 1).unwrap();
        e.map_callback(Mode::Normal, "Q", 1, None).unwrap();
        let request = callback(e.handle_key(Key::Char('Q')).unwrap());
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(-1)))
            .unwrap();
        let request = callback(e.handle_key(Key::Char('l')).unwrap());
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        keys(&mut e, "q");
        assert_eq!(e.register('a').unwrap().bytes, b"Ql");
    }
    #[test]
    fn input_interception_only_observes_unmapped_literal_insertion() {
        let mut e = editor(b"abc");
        e.set_input_interception(true);
        assert_eq!(
            e.handle_key(Key::Char('i')).unwrap(),
            EditorAction::Continue
        );
        let request = match e.handle_key(Key::Char('界')).unwrap() {
            EditorAction::Input(request) => request,
            _ => panic!("input event expected"),
        };
        assert_eq!(request.bytes, "界".as_bytes());
        assert!(!request.replace);
        assert_eq!(e.text.to_vec(), b"abc");
        e.resume_input(request.token, true).unwrap();
        assert_eq!(e.text.to_vec(), b"abc");
        assert!(e.resume_input(request.token, false).is_err());
        e.handle_key(Key::Ctrl('v')).unwrap();
        assert_eq!(
            e.handle_key(Key::Char('Z')).unwrap(),
            EditorAction::Continue
        );
        assert_eq!(e.text.to_vec(), b"Zabc");
        e.insert_raw(b"!", false).unwrap();
        assert_eq!(e.text.to_vec(), b"Z!abc");
        escape(&mut e);
        e.handle_key(Key::Char('R')).unwrap();
        let request = match e.handle_key(Key::Char('X')).unwrap() {
            EditorAction::Input(request) => request,
            _ => panic!("input event expected"),
        };
        assert!(request.replace);
        e.resume_input(request.token, false).unwrap();
        assert_eq!(e.text.to_vec(), b"Z!Xbc");
    }
    #[test]
    fn dot_repeat_replays_symbolic_mappings_and_waiting_callback_arguments() {
        let mut e = editor(b"one\ntwo\n");
        e.register_callback(CallbackType::Mapping, 2).unwrap();
        e.map_callback(Mode::Insert, "Q", 2, None).unwrap();
        keys(&mut e, "i");
        let request = callback(e.feed_keys(vec![Key::Char('Q')]).unwrap());
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(-1)))
            .unwrap();
        let request = callback(e.feed_keys(vec![Key::Char('a')]).unwrap());
        e.insert_raw("λ".as_bytes(), false).unwrap();
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        escape(&mut e);
        keys(&mut e, "j0");
        let request = callback(e.handle_key(Key::Char('.')).unwrap());
        assert_eq!(
            request.kind,
            CallbackKind::Mapping {
                keys: String::new()
            }
        );
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(-1)))
            .unwrap();
        let request = callback(e.next_queued().unwrap());
        assert_eq!(request.kind, CallbackKind::Mapping { keys: "a".into() });
        e.insert_raw("λ".as_bytes(), false).unwrap();
        e.resume_callback(request.token, CallbackResponse::Mapping(Some(1)))
            .unwrap();
        e.next_queued().unwrap();
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.text.to_vec(), "λone\nλtwo\n".as_bytes());
    }
    #[test]
    fn counted_insertion_and_dot_resume_every_input_event() {
        let mut e = editor(b"abc");
        e.set_input_interception(true);
        let mut events = 0;
        fn settle(e: &mut Editor, mut action: EditorAction, events: &mut usize) {
            loop {
                match action {
                    EditorAction::Input(request) => {
                        *events += 1;
                        e.resume_input(request.token, false).unwrap();
                        action = e.next_queued().unwrap();
                    }
                    EditorAction::Continue => break,
                    _ => panic!("input or completion expected"),
                }
            }
        }
        for key in e.parse_keys("3iX<Escape>").unwrap() {
            let action = e.handle_key(key).unwrap();
            settle(&mut e, action, &mut events);
        }
        assert_eq!(e.text.to_vec(), b"XXXabc");
        assert_eq!(events, 3);
        assert_eq!(e.mode, Mode::Normal);
        let action = e.handle_key(Key::Char('.')).unwrap();
        settle(&mut e, action, &mut events);
        assert_eq!(e.text.to_vec(), b"XXXXXXabc");
        assert_eq!(events, 6);
        assert_eq!(e.mode, Mode::Normal);
    }
    #[test]
    fn nested_macros_suspend_and_resume_literal_input_without_dropping_keys() {
        let mut e = editor(b"abc");
        e.set_input_interception(true);
        e.set_register_slots('a', vec![b"iX<Escape>".to_vec()])
            .unwrap();
        e.set_register_slots('b', vec![b"@a@a".to_vec()]).unwrap();
        keys(&mut e, "@");
        let mut action = e.handle_key(Key::Char('b')).unwrap();
        let mut events = 0;
        loop {
            match action {
                EditorAction::Input(request) => {
                    events += 1;
                    e.resume_input(request.token, false).unwrap();
                    action = e.next_queued().unwrap();
                }
                EditorAction::Continue => break,
                _ => panic!("input or completion expected"),
            }
        }
        assert_eq!(events, 2);
        assert_eq!(e.text.to_vec(), b"XXabc");
        assert_eq!(e.mode, Mode::Normal);
        assert_eq!(e.macro_depth, 0);
        assert_eq!(e.register('a').unwrap().bytes, b"iX<Escape>");
    }
    #[test]
    fn hosted_prompt_native_actions_follow_mapping_resolution() {
        let mut e = editor(b"one two");
        e.set_host_prompts(true);
        assert_eq!(
            e.handle_key(Key::Char(':')).unwrap(),
            EditorAction::PromptShow {
                title: b":".to_vec()
            }
        );
        assert_eq!(e.mode, Mode::Normal);
        e.set_window_id(Some(1));
        e.map_alias(Mode::Normal, "<Enter>", "<vis-prompt-enter>", Some(1))
            .unwrap();
        assert_eq!(
            e.handle_key(Key::Enter).unwrap(),
            EditorAction::PromptSubmit
        );
        assert_eq!(
            e.handle_key(Key::Ctrl('m')).unwrap(),
            EditorAction::PromptSubmit
        );
        e.map_alias(Mode::Insert, "<Enter>", "<vis-prompt-enter>", Some(1))
            .unwrap();
        e.set_mode_raw(Mode::Insert);
        assert_eq!(
            e.handle_key(Key::Ctrl('m')).unwrap(),
            EditorAction::PromptSubmit
        );
        e.set_mode_raw(Mode::Normal);
        e.map_alias(Mode::Normal, ":", "l", Some(1)).unwrap();
        assert_eq!(
            e.handle_key(Key::Char(':')).unwrap(),
            EditorAction::Continue
        );
        assert_eq!(e.cursor, 1);
    }
    #[test]
    fn prompt_scope_keeps_parent_parser_dot_and_default_register() {
        let mut e = editor(b"one two three");
        keys(&mut e, "dw");
        let previous = e.last_change.clone();
        let unnamed = e.register('"').unwrap().bytes.clone();
        keys(&mut e, "2d");
        let state = e.take_prompt_action_state();
        let parent_view = e.view_state();
        let parent_id = e.selection_ids()[0];
        let parent = std::mem::replace(&mut e.text, Text::from_bytes(b":write file\n"));
        let id = SelectionId::fresh().unwrap();
        e.install_view_state_raw(
            vec![(
                id,
                Selection {
                    anchor: 0,
                    cursor: 0,
                },
                false,
            )],
            id,
        )
        .unwrap();
        e.set_internal_scope(true);
        e.set_mode_raw(Mode::Normal);
        keys(&mut e, "dw");
        assert_eq!(e.register('"').unwrap().bytes, unnamed);
        assert_eq!(e.last_change, previous);
        keys(&mut e, "P");
        assert_eq!(e.text.to_vec(), b":write file\n");
        e.text = parent;
        e.install_view_state_raw(parent_view, parent_id).unwrap();
        e.set_internal_scope(false);
        e.restore_prompt_action_state(state);
        assert_eq!(e.mode, Mode::OperatorPending);
        assert_eq!(e.operator_count, 1);
        assert_eq!(e.count, Some(2));
        keys(&mut e, "w");
        assert_eq!(e.text.to_vec(), b"");
    }
    #[test]
    fn hosted_search_distinguishes_invalid_absent_and_empty_patterns() {
        let mut e = editor(b"one two one");
        e.set_host_prompts(true);
        keys(&mut e, "2d");
        assert_eq!(
            e.handle_key(Key::Char('/')).unwrap(),
            EditorAction::PromptShow {
                title: b"/".to_vec()
            }
        );
        let state = e.take_prompt_action_state();
        e.restore_prompt_action_state(state);
        assert!(e.submit_search("[", false).is_err());
        assert_eq!(e.text.to_vec(), b"one two one");
        assert_eq!(e.mode, Mode::OperatorPending);
        assert!(e.operator.is_none());
        assert_eq!(e.count, None);
        assert_eq!(e.submit_search("", false).unwrap(), EditorAction::Continue);
        assert_eq!(e.mode, Mode::OperatorPending);
        assert_eq!(
            e.submit_search("missing", false).unwrap(),
            EditorAction::Continue
        );
        assert_eq!(e.text.to_vec(), b"one two one");
        assert_eq!(e.mode, Mode::OperatorPending);
        assert_eq!(e.register('/').unwrap().bytes, b"missing");
    }
    #[test]
    fn recursive_aliases_reach_window_local_prompt_completion_and_bound_cycles() {
        let mut e = editor(b":file\n");
        e.set_host_prompts(true);
        e.set_internal_scope(true);
        e.set_window_id(Some(2));
        e.set_mode_raw(Mode::Insert);
        e.register_callback(CallbackType::Mapping, 8).unwrap();
        e.map_callback(Mode::Insert, "<C-x><C-o>", 8, None).unwrap();
        e.map_alias(Mode::Insert, "<Tab>", "<C-x><C-o>", Some(2))
            .unwrap();
        assert_eq!(callback(e.handle_key(Key::Tab).unwrap()).callback, 8);
        let mut e = editor(b"abc");
        e.map_alias(Mode::Normal, "Q", "Z", None).unwrap();
        e.map_alias(Mode::Normal, "Z", "Q", None).unwrap();
        assert!(
            e.handle_key(Key::Char('Q'))
                .unwrap_err()
                .0
                .contains("recursive mapping")
        );
        assert_eq!(e.text.to_vec(), b"abc");
    }
    #[test]
    fn hosted_search_replaces_parent_view_marks_with_its_new_cursor() {
        let mut e = editor(b"abc xyz abc");
        let id = e.selection_ids()[0];
        e.update_selection(
            id,
            Selection {
                anchor: 4,
                cursor: 4,
            },
        )
        .unwrap();
        let marks = e.selection_marks(id).unwrap();
        e.restore_selection_marks(vec![(id, marks)]).unwrap();
        assert_eq!(e.selection_position(id), Some(4));
        e.submit_search("abc", true).unwrap();
        assert_eq!(e.cursor, 0);
        assert_eq!(e.selection_position(id), Some(0));
        let marks = e.selection_marks(id).unwrap();
        e.restore_selection_marks(vec![(id, marks)]).unwrap();
        e.execute_command("#0,#3c/longer/").unwrap();
        assert!(e.selection_position(id).is_some());
    }
    #[test]
    fn recognized_command_handler_failures_are_messages_but_parse_errors_stay_errors() {
        let mut e = editor(b"one two\n");
        assert!(e.execute_command("'ap").is_err());
        assert!(e.execute_command("map! insert jj").is_err());
        assert!(e.execute_command("set unknown-option").is_err());
        assert!(matches!(
            e.prepare_command("'ap").unwrap(),
            EditorAction::Message(_)
        ));
        assert!(e.prepare_command("not-a-command").is_err());
        assert!(e.prepare_command("x/[/p").is_err());
        assert!(matches!(
            e.prepare_command("map! insert jj").unwrap(),
            EditorAction::Message(_)
        ));
        assert!(e.mappings.is_empty());
        assert!(matches!(
            e.prepare_command("map nonsense q x").unwrap(),
            EditorAction::Message(_)
        ));
        assert!(matches!(
            e.prepare_command("set tabwidth=0").unwrap(),
            EditorAction::Message(_)
        ));
        assert!(matches!(
            e.prepare_command("set unknown-option").unwrap(),
            EditorAction::Message(_)
        ));
        e.prepare_command("map! insert jj <vis-nop>").unwrap();
        assert_eq!(e.mappings.len(), 1);
        assert_eq!(e.text.to_vec(), b"one two\n");
    }
    #[test]
    fn interactive_evaluation_failure_does_not_weaken_sam_transaction_errors() {
        let mut e = editor(b"ab");
        assert!(matches!(
            e.prepare_command("#99p").unwrap(),
            EditorAction::Message(_)
        ));
        assert_eq!(e.text.to_vec(), b"ab");
        assert!(e.prepare_command("{#0,#1c/X/ #0,#2c/Y/}").is_err());
        assert_eq!(e.text.to_vec(), b"ab");
    }
    #[test]
    fn catalog_recognizes_every_c_name_without_registering_missing_operations_as_noops() {
        for definition in NATIVE_ACTIONS {
            assert_eq!(
                Editor::notation_keys(&format!("<{}>", definition.name)).unwrap(),
                vec![Key::Native(definition.action)]
            );
        }
        let mut e = editor(b"text");
        let before = e.text.to_vec();
        let action = NativeAction::EditorSuspend;
        assert_eq!(action.support(), NativeSupport::Implemented);
        assert_eq!(
            e.handle_key(Key::Native(action)).unwrap(),
            EditorAction::HostCommand("suspend".into())
        );
        assert_eq!(
            e.handle_key(Key::Ctrl('z')).unwrap(),
            EditorAction::HostCommand("suspend".into())
        );
        assert_eq!(e.text.to_vec(), before);
    }
    #[test]
    fn catalog_support_and_visual_line_geometry_are_explicit() {
        let mut implemented = 0;
        let mut conditional = 0;
        let mut unsupported = 0;
        for definition in NATIVE_ACTIONS {
            match definition.action.support() {
                NativeSupport::Implemented => implemented += 1,
                NativeSupport::Conditional(_) => conditional += 1,
                NativeSupport::Unsupported(reason) => {
                    unsupported += 1;
                    let mut e = editor(b"one two\nthree four\n");
                    let before = e.view_state();
                    let error = e.invoke_native_action(definition.action).unwrap_err();
                    assert!(error.0.contains(definition.name));
                    assert!(error.0.contains(reason));
                    assert_eq!(e.text.to_vec(), b"one two\nthree four\n");
                    assert_eq!(e.view_state(), before);
                }
            }
        }
        assert_eq!(implemented + conditional + unsupported, 178);
        eprintln!(
            "native catalog: {implemented} implemented, {conditional} conditional, {unsupported} unsupported"
        );
        let mut e = editor(b"one two\nthree four\n");
        keys(&mut e, "j");
        let id = e.selection_ids()[0];
        e.handle_key(Key::Native(NativeAction::ModeVisualLine))
            .unwrap();
        assert_eq!(
            e.selections()[0],
            Selection {
                anchor: 8,
                cursor: 18
            }
        );
        e.handle_key(Key::Native(NativeAction::CursorLineUp))
            .unwrap();
        assert_eq!(
            e.selections()[0],
            Selection {
                anchor: 18,
                cursor: 0
            }
        );
        assert_eq!(e.selection_ids()[0], id);
        // Raw view installation does not apply the global visual-line mode to
        // another window's preexisting characterwise selection.
        e.install_view_state_raw(
            vec![(
                id,
                Selection {
                    anchor: 1,
                    cursor: 2,
                },
                true,
            )],
            id,
        )
        .unwrap();
        assert_eq!(e.selection_ranges(), vec![1..3]);
        keys(&mut e, "l");
        assert_eq!(
            e.selections()[0],
            Selection {
                anchor: 0,
                cursor: 7
            }
        );
    }
    #[test]
    fn line_operator_plan_is_atomic_preserves_registers_and_tracks_undo_marks() {
        let mut e = editor(b"one two\nthree four\n");
        e.execute_command("x/[a-z]+/").unwrap();
        e.set_register_slots('a', vec![b"saved".to_vec()]).unwrap();
        e.handle_key(Key::Native(NativeAction::OperatorShiftRight))
            .unwrap();
        assert_eq!(e.text.to_vec(), b"\t\tone two\n\t\tthree four\n");
        assert_eq!(e.get_register_slots('a'), vec![b"saved".to_vec()]);
        assert_eq!(
            e.selections().iter().map(|s| s.cursor).collect::<Vec<_>>(),
            vec![4, 8, 16, 21]
        );
        let before = e.text.to_vec();
        let view = e.view_state();
        let mut calls = 0;
        let result = e.run_line_operator(None, |plan, _, cursor| {
            calls += 1;
            if calls == 1 {
                plan.replace(0..0, b"x")?;
            } else {
                let invalid = plan.text.len() + 1;
                plan.replace(invalid..invalid, b"y")?;
            }
            Ok(cursor)
        });
        assert!(result.is_err());
        assert_eq!(e.text.to_vec(), before);
        assert_eq!(e.view_state(), view);
        let mut e = editor(b"one\ntwo\nthree\nfour\n");
        e.handle_key(Key::Ctrl('j')).unwrap();
        let ids = e.selection_ids().to_vec();
        keys(&mut e, "J");
        assert_eq!(e.text.to_vec(), b"one two three\nfour\n");
        assert_eq!(e.selection_ids(), ids);
        keys(&mut e, "u");
        assert_eq!(e.text.to_vec(), b"one\ntwo\nthree\nfour\n");
        assert!(ids.iter().all(|id| e.selection_position(*id).is_none()));
        e.handle_key(Key::Ctrl('r')).unwrap();
        assert_eq!(e.text.to_vec(), b"one two three\nfour\n");
        assert_eq!(
            ids.iter()
                .map(|id| e.selection_position(*id))
                .collect::<Vec<_>>(),
            vec![Some(3), Some(7)]
        );
    }
    #[test]
    fn duplicate_line_cursor_creation_focuses_the_adjacent_existing_object() {
        let mut e = editor(b"one\ntwo\nthree\n");
        keys(&mut e, "vGI");
        let ids = e.selection_ids().to_vec();
        assert_eq!(e.cursor, 8);
        e.handle_key(Key::Ctrl('k')).unwrap();
        assert_eq!(e.cursor, 4);
        assert_eq!(e.selection_ids(), ids);
        e.handle_key(Key::Ctrl('p')).unwrap();
        assert_eq!(e.cursor, 8);
        assert_eq!(e.selection_ids(), vec![ids[0], ids[2]]);
    }
    #[test]
    fn language_translation_is_physical_preserves_modifiers_and_literal_arguments() {
        let mut e = editor(b"abcd\n");
        e.execute_command("langmap д l").unwrap();
        e.handle_key(Key::Char('д')).unwrap();
        assert_eq!(e.cursor, 1);
        e.map_alias(Mode::Normal, "l", "h", None).unwrap();
        e.handle_key(Key::Char('д')).unwrap();
        assert_eq!(e.cursor, 0);
        assert_eq!(e.translate_language_key(Key::Alt('д')), Key::Alt('l'));
        assert_eq!(e.translate_language_key(Key::Ctrl('д')), Key::Ctrl('l'));
        assert_eq!(
            e.translate_language_key(Key::AltCtrl('д')),
            Key::AltCtrl('l')
        );
        assert!(e.execute_command("langmap дЖ xj").is_err());
        assert_eq!(e.language_map.get(&'д'), Some(&'l'));
        assert_eq!(e.language_map.get(&'Ж'), Some(&'j'));
        keys(&mut e, "iд");
        escape(&mut e);
        assert_eq!(e.text.to_vec(), "дabcd\n".as_bytes());
        let mut e = editor("aд bд\n".as_bytes());
        e.execute_command("langmap д l").unwrap();
        e.feed_keys(vec![Key::Char('f')]).unwrap();
        e.feed_keys(vec![Key::Char('д')]).unwrap();
        assert_eq!(e.cursor, 1);
        assert!(e.language_map_disabled);
        assert!(e.handle_key(Key::Char('д')).is_err());
        assert!(!e.language_map_disabled);
        e.handle_key(Key::Char('д')).unwrap();
        assert_eq!(e.cursor, 3);
        let mut e = editor(b"abcd\n");
        e.execute_command("langmap д l").unwrap();
        keys(&mut e, "qaдq");
        assert_eq!(e.register('a').unwrap().bytes, b"l");
        e.execute_command("langmap l h").unwrap();
        e.play_macro('a', 1).unwrap();
        assert_eq!(e.cursor, 2);
    }
    #[test]
    fn native_and_dynamic_action_collisions_refuse_overwrite_and_maps_take_precedence() {
        let mut e = editor(b"abc");
        e.register_callback(CallbackType::Mapping, 1).unwrap();
        assert!(e.action_exists("vis-operator-delete"));
        assert!(e.register_action("vis-operator-delete", 1).is_err());
        e.register_action("custom-action", 1).unwrap();
        assert!(e.register_action("custom-action", 1).is_err());
        e.map_alias(Mode::Normal, "<custom-action>", "l", None)
            .unwrap();
        assert_eq!(
            e.feed_keys(e.parse_keys("<custom-action>").unwrap())
                .unwrap(),
            EditorAction::Continue
        );
        assert_eq!(e.cursor, 1);
        e.map_alias(Mode::Normal, "<vis-motion-char-next>", "h", None)
            .unwrap();
        e.feed_keys(e.parse_keys("<vis-motion-char-next>").unwrap())
            .unwrap();
        assert_eq!(e.cursor, 0);
    }
    #[test]
    fn copying_local_maps_preserves_callbacks_and_allocated_empty_table_semantics() {
        let mut e = editor(b"abc");
        e.register_callback(CallbackType::Mapping, 9).unwrap();
        e.map_callback(Mode::Normal, "Q", 9, Some(1)).unwrap();
        e.map_alias(Mode::Insert, "X", "Y", Some(2)).unwrap();
        e.map_alias(Mode::Normal, "Q", "l", Some(2)).unwrap();
        e.copy_window_mappings(1, 2);
        e.set_window_id(Some(2));
        assert_eq!(callback(e.handle_key(Key::Char('Q')).unwrap()).callback, 9);
        assert_eq!(e.mapping_aliases(Mode::Insert, Some(2)).len(), 1);
        e.unmap(Mode::Normal, "Q", Some(1)).unwrap();
        e.copy_window_mappings(1, 2);
        assert!(e.mapping_aliases(Mode::Normal, Some(2)).is_empty());
        assert_eq!(e.mapping_aliases(Mode::Insert, Some(2)).len(), 1);
        e.forget_window_mappings(2);
        assert!(e.mapping_aliases(Mode::Insert, Some(2)).is_empty());
    }
}

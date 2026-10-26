//! Named native actions from `main.c` KEY_ACTION_LIST.
//! Handler families and arguments mirror the native C table, without key aliases.
use super::Mode;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NativeHandler {
    Call,
    Count,
    Delete,
    Earlier,
    Gotoline,
    InsertRegister,
    InsertVerbatim,
    Insertmode,
    Join,
    Jumplist,
    Later,
    MacroRecord,
    MacroReplay,
    Mark,
    Movement,
    MovementKey,
    Nop,
    NormalmodeEscape,
    Openline,
    Operator,
    Percent,
    PromptShow,
    Redo,
    Reg,
    Repeat,
    Replace,
    Replacemode,
    SelectionEnd,
    SelectionsAlign,
    SelectionsAlignIndent,
    SelectionsCase,
    SelectionsClear,
    SelectionsComplement,
    SelectionsIntersect,
    SelectionsMatchNext,
    SelectionsMatchSkip,
    SelectionsMinus,
    SelectionsNavigate,
    SelectionsNew,
    SelectionsRemove,
    SelectionsRemoveColumn,
    SelectionsRemoveColumnExcept,
    SelectionsRestore,
    SelectionsRotate,
    SelectionsSave,
    SelectionsTrim,
    SelectionsUnion,
    Suspend,
    Switchmode,
    TextObject,
    Undo,
    UnicodeInfo,
    VisualmodeEscape,
    Window,
    Wscroll,
    Wslide,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NativeMotion {
    BlockEnd,
    BlockStart,
    Byte,
    ByteLeft,
    ByteRight,
    CharNext,
    CharPrev,
    CodepointNext,
    CodepointPrev,
    Column,
    LineBegin,
    LineCharNext,
    LineCharPrev,
    LineDown,
    LineEnd,
    LineFinish,
    LineStart,
    LineUp,
    LongwordEndNext,
    LongwordEndPrev,
    LongwordStartNext,
    LongwordStartPrev,
    Nop,
    ParagraphNext,
    ParagraphPrev,
    ParenthesisEnd,
    ParenthesisStart,
    ScreenLineBegin,
    ScreenLineDown,
    ScreenLineEnd,
    ScreenLineMiddle,
    ScreenLineUp,
    SearchRepeat,
    SearchRepeatBackward,
    SearchRepeatForward,
    SearchRepeatReverse,
    SearchWordBackward,
    SearchWordForward,
    SentenceNext,
    SentencePrev,
    TillLeft,
    TillLineLeft,
    TillLineRight,
    TillRight,
    TotillRepeat,
    TotillReverse,
    ToLeft,
    ToLineLeft,
    ToLineRight,
    ToRight,
    WindowLineBottom,
    WindowLineMiddle,
    WindowLineTop,
    WordEndNext,
    WordEndPrev,
    WordStartNext,
    WordStartPrev,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NativeOperator {
    Change,
    CursorEol,
    CursorSol,
    Delete,
    PutAfter,
    PutBefore,
    ShiftLeft,
    ShiftRight,
    Yank,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NativeTextObject {
    Indentation,
    InnerAngleBracket,
    InnerBacktick,
    InnerCurlyBracket,
    InnerLine,
    InnerLongword,
    InnerParenthesis,
    InnerQuote,
    InnerSingleQuote,
    InnerSquareBracket,
    InnerWord,
    OuterAngleBracket,
    OuterBacktick,
    OuterCurlyBracket,
    OuterLine,
    OuterLongword,
    OuterParenthesis,
    OuterQuote,
    OuterSingleQuote,
    OuterSquareBracket,
    OuterWord,
    Paragraph,
    ParagraphOuter,
    SearchBackward,
    SearchForward,
    Sentence,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeCall {
    InsertNewline,
    InsertTab,
    Redraw,
    WindowNext,
    WindowPrev,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeWindow {
    Bottom,
    Center,
    Top,
}
/// Native window scrolling actions. Native `view_scroll_page_up`,
/// `view_scroll_halfpage_up`, `view_slide_up` and their counterparts all
/// move the view and the cursor together.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum NativeScroll {
    PageDown,
    PageUp,
    HalfPageDown,
    HalfPageUp,
    SlideDown,
    SlideUp,
    /// Native `view_scroll_down`: scroll the view without centering.
    ScrollDown,
    /// Native `view_scroll_up`: scroll the view without centering.
    ScrollUp,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeInfo {
    Unicode,
    Utf8,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeArgument {
    None,
    Motion(NativeMotion),
    Operator(NativeOperator),
    TextObject(NativeTextObject),
    Call(NativeCall),
    Window(NativeWindow),
    Scroll(NativeScroll),
    Mode(Mode),
    Integer(i64),
    Text(&'static str),
    Boolean(bool),
    Info(NativeInfo),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(usize)]
pub enum NativeAction {
    InsertNewline,
    InsertTab,
    Redraw,
    WindowNext,
    WindowPrev,
    Count,
    DeleteCharNext,
    DeleteCharPrev,
    DeleteLineBegin,
    DeleteWordPrev,
    Earlier,
    CursorLineFirst,
    CursorLineLast,
    InsertRegister,
    InsertVerbatim,
    AppendCharNext,
    AppendLineEnd,
    InsertLineStart,
    ModeInsert,
    JoinLines,
    JoinLinesTrim,
    JumplistNext,
    JumplistPrev,
    JumplistSave,
    Later,
    MacroRecord,
    MacroReplay,
    Mark,
    CursorBlockEnd,
    CursorBlockStart,
    CursorByte,
    CursorByteLeft,
    CursorByteRight,
    CursorCharNext,
    CursorCharPrev,
    CursorCodepointNext,
    CursorCodepointPrev,
    CursorColumn,
    CursorLineBegin,
    CursorLineCharNext,
    CursorLineCharPrev,
    CursorLineDown,
    CursorLineEnd,
    CursorLineFinish,
    CursorLineStart,
    CursorLineUp,
    CursorLongwordEndNext,
    CursorLongwordEndPrev,
    CursorLongwordStartNext,
    CursorLongwordStartPrev,
    CursorParagraphNext,
    CursorParagraphPrev,
    CursorParenthesisEnd,
    CursorParenthesisStart,
    CursorScreenLineBegin,
    CursorScreenLineDown,
    CursorScreenLineEnd,
    CursorScreenLineMiddle,
    CursorScreenLineUp,
    CursorSearchRepeat,
    CursorSearchRepeatBackward,
    CursorSearchRepeatForward,
    CursorSearchRepeatReverse,
    CursorSearchWordBackward,
    CursorSearchWordForward,
    CursorSentenceNext,
    CursorSentencePrev,
    CursorWindowLineBottom,
    CursorWindowLineMiddle,
    CursorWindowLineTop,
    CursorWordEndNext,
    CursorWordEndPrev,
    CursorWordStartNext,
    CursorWordStartPrev,
    TotillRepeat,
    TotillReverse,
    TillLeft,
    TillLineLeft,
    TillLineRight,
    TillRight,
    ToLeft,
    ToLineLeft,
    ToLineRight,
    ToRight,
    Nop,
    ModeNormalEscape,
    OpenLineAbove,
    OpenLineBelow,
    OperatorChange,
    OperatorDelete,
    OperatorShiftLeft,
    OperatorShiftRight,
    OperatorYank,
    PutAfter,
    PutBefore,
    SelectionsNewLinesBegin,
    SelectionsNewLinesEnd,
    CursorPercent,
    PromptSearchBackward,
    PromptSearchForward,
    PromptShow,
    Redo,
    Register,
    Repeat,
    ReplaceChar,
    ModeReplace,
    SelectionFlip,
    SelectionsAlign,
    SelectionsAlignIndentLeft,
    SelectionsAlignIndentRight,
    SelectionsCaseToupper,
    SelectionsCaseToggle,
    SelectionsCaseTolower,
    SelectionsRemoveAll,
    SelectionsComplement,
    SelectionsIntersect,
    SelectionsNewMatchAll,
    SelectionsNewMatchNext,
    SelectionsNewMatchSkip,
    SelectionsMinus,
    SelectionsNext,
    SelectionsPrev,
    SelectionsNewLineAbove,
    SelectionsNewLineAboveFirst,
    SelectionsNewLineBelow,
    SelectionsNewLineBelowLast,
    SelectionsRemoveLast,
    SelectionsRemoveColumn,
    SelectionsRemoveColumnExcept,
    SelectionsRestore,
    SelectionsRotateLeft,
    SelectionsRotateRight,
    SelectionsSave,
    SelectionsTrim,
    SelectionsUnion,
    EditorSuspend,
    ModeNormal,
    ModeVisual,
    ModeVisualLine,
    TextObjectAngleBracketInner,
    TextObjectAngleBracketOuter,
    TextObjectBacktickInner,
    TextObjectBacktickOuter,
    TextObjectCurlyBracketInner,
    TextObjectCurlyBracketOuter,
    TextObjectIndentation,
    TextObjectLineInner,
    TextObjectLineOuter,
    TextObjectLongwordInner,
    TextObjectLongwordOuter,
    TextObjectParagraph,
    TextObjectParagraphOuter,
    TextObjectParenthesisInner,
    TextObjectParenthesisOuter,
    TextObjectQuoteInner,
    TextObjectQuoteOuter,
    TextObjectSearchBackward,
    TextObjectSearchForward,
    TextObjectSentence,
    TextObjectSingleQuoteInner,
    TextObjectSingleQuoteOuter,
    TextObjectSquareBracketInner,
    TextObjectSquareBracketOuter,
    TextObjectWordInner,
    TextObjectWordOuter,
    Undo,
    UnicodeInfo,
    Utf8Info,
    ModeVisualEscape,
    WindowRedrawBottom,
    WindowRedrawCenter,
    WindowRedrawTop,
    WindowHalfpageDown,
    WindowHalfpageUp,
    WindowPageDown,
    WindowPageUp,
    WindowSlideDown,
    WindowSlideUp,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeActionDefinition {
    pub action: NativeAction,
    pub name: &'static str,
    pub handler: NativeHandler,
    pub argument: NativeArgument,
    pub help: &'static str,
}
pub const NATIVE_ACTIONS: &[NativeActionDefinition] = &[
    NativeActionDefinition {
        action: NativeAction::InsertNewline,
        name: "vis-insert-newline",
        handler: NativeHandler::Call,
        argument: NativeArgument::Call(NativeCall::InsertNewline),
        help: "Insert a line break (depending on file type)",
    },
    NativeActionDefinition {
        action: NativeAction::InsertTab,
        name: "vis-insert-tab",
        handler: NativeHandler::Call,
        argument: NativeArgument::Call(NativeCall::InsertTab),
        help: "Insert a tab (might be converted to spaces)",
    },
    NativeActionDefinition {
        action: NativeAction::Redraw,
        name: "vis-redraw",
        handler: NativeHandler::Call,
        argument: NativeArgument::Call(NativeCall::Redraw),
        help: "Redraw current editor content",
    },
    NativeActionDefinition {
        action: NativeAction::WindowNext,
        name: "vis-window-next",
        handler: NativeHandler::Call,
        argument: NativeArgument::Call(NativeCall::WindowNext),
        help: "Focus next window",
    },
    NativeActionDefinition {
        action: NativeAction::WindowPrev,
        name: "vis-window-prev",
        handler: NativeHandler::Call,
        argument: NativeArgument::Call(NativeCall::WindowPrev),
        help: "Focus previous window",
    },
    NativeActionDefinition {
        action: NativeAction::Count,
        name: "vis-count",
        handler: NativeHandler::Count,
        argument: NativeArgument::None,
        help: "Count specifier",
    },
    NativeActionDefinition {
        action: NativeAction::DeleteCharNext,
        name: "vis-delete-char-next",
        handler: NativeHandler::Delete,
        argument: NativeArgument::Motion(NativeMotion::CharNext),
        help: "Delete the next character",
    },
    NativeActionDefinition {
        action: NativeAction::DeleteCharPrev,
        name: "vis-delete-char-prev",
        handler: NativeHandler::Delete,
        argument: NativeArgument::Motion(NativeMotion::CharPrev),
        help: "Delete the previous character",
    },
    NativeActionDefinition {
        action: NativeAction::DeleteLineBegin,
        name: "vis-delete-line-begin",
        handler: NativeHandler::Delete,
        argument: NativeArgument::Motion(NativeMotion::LineBegin),
        help: "Delete until the start of the current line",
    },
    NativeActionDefinition {
        action: NativeAction::DeleteWordPrev,
        name: "vis-delete-word-prev",
        handler: NativeHandler::Delete,
        argument: NativeArgument::Motion(NativeMotion::WordStartPrev),
        help: "Delete the previous WORD",
    },
    NativeActionDefinition {
        action: NativeAction::Earlier,
        name: "vis-earlier",
        handler: NativeHandler::Earlier,
        argument: NativeArgument::None,
        help: "Goto older text state",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineFirst,
        name: "vis-motion-line-first",
        handler: NativeHandler::Gotoline,
        argument: NativeArgument::Integer(-1),
        help: "Move cursor to given line (defaults to first)",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineLast,
        name: "vis-motion-line-last",
        handler: NativeHandler::Gotoline,
        argument: NativeArgument::Integer(1),
        help: "Move cursor to given line (defaults to last)",
    },
    NativeActionDefinition {
        action: NativeAction::InsertRegister,
        name: "vis-insert-register",
        handler: NativeHandler::InsertRegister,
        argument: NativeArgument::None,
        help: "Insert specified register content",
    },
    NativeActionDefinition {
        action: NativeAction::InsertVerbatim,
        name: "vis-insert-verbatim",
        handler: NativeHandler::InsertVerbatim,
        argument: NativeArgument::None,
        help: "Insert Unicode character based on code point",
    },
    NativeActionDefinition {
        action: NativeAction::AppendCharNext,
        name: "vis-append-char-next",
        handler: NativeHandler::Insertmode,
        argument: NativeArgument::Motion(NativeMotion::LineCharNext),
        help: "Append text after the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::AppendLineEnd,
        name: "vis-append-line-end",
        handler: NativeHandler::Insertmode,
        argument: NativeArgument::Motion(NativeMotion::LineEnd),
        help: "Append text after the end of the line",
    },
    NativeActionDefinition {
        action: NativeAction::InsertLineStart,
        name: "vis-insert-line-start",
        handler: NativeHandler::Insertmode,
        argument: NativeArgument::Motion(NativeMotion::LineStart),
        help: "Insert text before the first non-blank in the line",
    },
    NativeActionDefinition {
        action: NativeAction::ModeInsert,
        name: "vis-mode-insert",
        handler: NativeHandler::Insertmode,
        argument: NativeArgument::Motion(NativeMotion::Nop),
        help: "Enter insert mode",
    },
    NativeActionDefinition {
        action: NativeAction::JoinLines,
        name: "vis-join-lines",
        handler: NativeHandler::Join,
        argument: NativeArgument::Text(" "),
        help: "Join selected lines",
    },
    NativeActionDefinition {
        action: NativeAction::JoinLinesTrim,
        name: "vis-join-lines-trim",
        handler: NativeHandler::Join,
        argument: NativeArgument::Text(""),
        help: "Join selected lines, remove white space",
    },
    NativeActionDefinition {
        action: NativeAction::JumplistNext,
        name: "vis-jumplist-next",
        handler: NativeHandler::Jumplist,
        argument: NativeArgument::Integer(1),
        help: "Go to newer cursor position in jump list",
    },
    NativeActionDefinition {
        action: NativeAction::JumplistPrev,
        name: "vis-jumplist-prev",
        handler: NativeHandler::Jumplist,
        argument: NativeArgument::Integer(-1),
        help: "Go to older cursor position in jump list",
    },
    NativeActionDefinition {
        action: NativeAction::JumplistSave,
        name: "vis-jumplist-save",
        handler: NativeHandler::Jumplist,
        argument: NativeArgument::Integer(0),
        help: "Save current selections in jump list",
    },
    NativeActionDefinition {
        action: NativeAction::Later,
        name: "vis-later",
        handler: NativeHandler::Later,
        argument: NativeArgument::None,
        help: "Goto newer text state",
    },
    NativeActionDefinition {
        action: NativeAction::MacroRecord,
        name: "vis-macro-record",
        handler: NativeHandler::MacroRecord,
        argument: NativeArgument::None,
        help: "Record macro into given register",
    },
    NativeActionDefinition {
        action: NativeAction::MacroReplay,
        name: "vis-macro-replay",
        handler: NativeHandler::MacroReplay,
        argument: NativeArgument::None,
        help: "Replay macro, execute the content of the given register",
    },
    NativeActionDefinition {
        action: NativeAction::Mark,
        name: "vis-mark",
        handler: NativeHandler::Mark,
        argument: NativeArgument::None,
        help: "Use given mark for next action",
    },
    NativeActionDefinition {
        action: NativeAction::CursorBlockEnd,
        name: "vis-motion-block-end",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::BlockEnd),
        help: "Move cursor to the closing curly brace in a block",
    },
    NativeActionDefinition {
        action: NativeAction::CursorBlockStart,
        name: "vis-motion-block-start",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::BlockStart),
        help: "Move cursor to the opening curly brace in a block",
    },
    NativeActionDefinition {
        action: NativeAction::CursorByte,
        name: "vis-motion-byte",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::Byte),
        help: "Move to absolute byte position",
    },
    NativeActionDefinition {
        action: NativeAction::CursorByteLeft,
        name: "vis-motion-byte-left",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ByteLeft),
        help: "Move count bytes to the left",
    },
    NativeActionDefinition {
        action: NativeAction::CursorByteRight,
        name: "vis-motion-byte-right",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ByteRight),
        help: "Move count bytes to the right",
    },
    NativeActionDefinition {
        action: NativeAction::CursorCharNext,
        name: "vis-motion-char-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::CharNext),
        help: "Move cursor right, to the next character",
    },
    NativeActionDefinition {
        action: NativeAction::CursorCharPrev,
        name: "vis-motion-char-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::CharPrev),
        help: "Move cursor left, to the previous character",
    },
    NativeActionDefinition {
        action: NativeAction::CursorCodepointNext,
        name: "vis-motion-codepoint-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::CodepointNext),
        help: "Move to the next Unicode codepoint",
    },
    NativeActionDefinition {
        action: NativeAction::CursorCodepointPrev,
        name: "vis-motion-codepoint-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::CodepointPrev),
        help: "Move to the previous Unicode codepoint",
    },
    NativeActionDefinition {
        action: NativeAction::CursorColumn,
        name: "vis-motion-column",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::Column),
        help: "Move cursor to given column of current line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineBegin,
        name: "vis-motion-line-begin",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineBegin),
        help: "Move cursor to first character of the line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineCharNext,
        name: "vis-motion-line-char-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineCharNext),
        help: "Move cursor right, to the next character on the same line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineCharPrev,
        name: "vis-motion-line-char-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineCharPrev),
        help: "Move cursor left,  to the previous character on the same line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineDown,
        name: "vis-motion-line-down",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineDown),
        help: "Move cursor line downwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineEnd,
        name: "vis-motion-line-end",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineEnd),
        help: "Move cursor to end of the line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineFinish,
        name: "vis-motion-line-finish",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineFinish),
        help: "Move cursor to last non-blank character of the line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineStart,
        name: "vis-motion-line-start",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineStart),
        help: "Move cursor to first non-blank character of the line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLineUp,
        name: "vis-motion-line-up",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LineUp),
        help: "Move cursor line upwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLongwordEndNext,
        name: "vis-motion-bigword-end-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LongwordEndNext),
        help: "Move cursor forward to the end of WORD",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLongwordEndPrev,
        name: "vis-motion-bigword-end-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LongwordEndPrev),
        help: "Move cursor backwards to the end of WORD",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLongwordStartNext,
        name: "vis-motion-bigword-start-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LongwordStartNext),
        help: "Move cursor WORDS forwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorLongwordStartPrev,
        name: "vis-motion-bigword-start-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::LongwordStartPrev),
        help: "Move cursor WORDS backwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorParagraphNext,
        name: "vis-motion-paragraph-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ParagraphNext),
        help: "Move cursor paragraph forward",
    },
    NativeActionDefinition {
        action: NativeAction::CursorParagraphPrev,
        name: "vis-motion-paragraph-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ParagraphPrev),
        help: "Move cursor paragraph backward",
    },
    NativeActionDefinition {
        action: NativeAction::CursorParenthesisEnd,
        name: "vis-motion-parenthesis-end",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ParenthesisEnd),
        help: "Move cursor to the closing parenthesis inside a pair of parentheses",
    },
    NativeActionDefinition {
        action: NativeAction::CursorParenthesisStart,
        name: "vis-motion-parenthesis-start",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ParenthesisStart),
        help: "Move cursor to the opening parenthesis inside a pair of parentheses",
    },
    NativeActionDefinition {
        action: NativeAction::CursorScreenLineBegin,
        name: "vis-motion-screenline-begin",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ScreenLineBegin),
        help: "Move cursor to beginning of screen/display line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorScreenLineDown,
        name: "vis-motion-screenline-down",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ScreenLineDown),
        help: "Move cursor screen/display line downwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorScreenLineEnd,
        name: "vis-motion-screenline-end",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ScreenLineEnd),
        help: "Move cursor to end of screen/display line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorScreenLineMiddle,
        name: "vis-motion-screenline-middle",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ScreenLineMiddle),
        help: "Move cursor to middle of screen/display line",
    },
    NativeActionDefinition {
        action: NativeAction::CursorScreenLineUp,
        name: "vis-motion-screenline-up",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::ScreenLineUp),
        help: "Move cursor screen/display line upwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchRepeat,
        name: "vis-motion-search-repeat",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchRepeat),
        help: "Move cursor to next match",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchRepeatBackward,
        name: "vis-motion-search-repeat-backward",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchRepeatBackward),
        help: "Move cursor to previous match in backward direction",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchRepeatForward,
        name: "vis-motion-search-repeat-forward",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchRepeatForward),
        help: "Move cursor to next match in forward direction",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchRepeatReverse,
        name: "vis-motion-search-repeat-reverse",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchRepeatReverse),
        help: "Move cursor to next match in opposite direction",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchWordBackward,
        name: "vis-motion-search-word-backward",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchWordBackward),
        help: "Move cursor to previous occurrence of the word under cursor",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSearchWordForward,
        name: "vis-motion-search-word-forward",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SearchWordForward),
        help: "Move cursor to next occurrence of the word under cursor",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSentenceNext,
        name: "vis-motion-sentence-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SentenceNext),
        help: "Move cursor sentence forward",
    },
    NativeActionDefinition {
        action: NativeAction::CursorSentencePrev,
        name: "vis-motion-sentence-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::SentencePrev),
        help: "Move cursor sentence backward",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWindowLineBottom,
        name: "vis-motion-window-line-bottom",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WindowLineBottom),
        help: "Move cursor to bottom line of the window",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWindowLineMiddle,
        name: "vis-motion-window-line-middle",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WindowLineMiddle),
        help: "Move cursor to middle line of the window",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWindowLineTop,
        name: "vis-motion-window-line-top",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WindowLineTop),
        help: "Move cursor to top line of the window",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWordEndNext,
        name: "vis-motion-word-end-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WordEndNext),
        help: "Move cursor forward to the end of word",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWordEndPrev,
        name: "vis-motion-word-end-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WordEndPrev),
        help: "Move cursor backwards to the end of word",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWordStartNext,
        name: "vis-motion-word-start-next",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WordStartNext),
        help: "Move cursor words forwards",
    },
    NativeActionDefinition {
        action: NativeAction::CursorWordStartPrev,
        name: "vis-motion-word-start-prev",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::WordStartPrev),
        help: "Move cursor words backwards",
    },
    NativeActionDefinition {
        action: NativeAction::TotillRepeat,
        name: "vis-motion-totill-repeat",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::TotillRepeat),
        help: "Repeat latest to/till motion",
    },
    NativeActionDefinition {
        action: NativeAction::TotillReverse,
        name: "vis-motion-totill-reverse",
        handler: NativeHandler::Movement,
        argument: NativeArgument::Motion(NativeMotion::TotillReverse),
        help: "Repeat latest to/till motion but in opposite direction",
    },
    NativeActionDefinition {
        action: NativeAction::TillLeft,
        name: "vis-motion-till-left",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::TillLeft),
        help: "Till after the occurrence of character to the left",
    },
    NativeActionDefinition {
        action: NativeAction::TillLineLeft,
        name: "vis-motion-till-line-left",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::TillLineLeft),
        help: "Till after the occurrence of character to the left on the current line",
    },
    NativeActionDefinition {
        action: NativeAction::TillLineRight,
        name: "vis-motion-till-line-right",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::TillLineRight),
        help: "Till before the occurrence of character to the right on the current line",
    },
    NativeActionDefinition {
        action: NativeAction::TillRight,
        name: "vis-motion-till-right",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::TillRight),
        help: "Till before the occurrence of character to the right",
    },
    NativeActionDefinition {
        action: NativeAction::ToLeft,
        name: "vis-motion-to-left",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::ToLeft),
        help: "To the first occurrence of character to the left",
    },
    NativeActionDefinition {
        action: NativeAction::ToLineLeft,
        name: "vis-motion-to-line-left",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::ToLineLeft),
        help: "To the first occurrence of character to the left on the current line",
    },
    NativeActionDefinition {
        action: NativeAction::ToLineRight,
        name: "vis-motion-to-line-right",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::ToLineRight),
        help: "To the first occurrence of character to the right on the current line",
    },
    NativeActionDefinition {
        action: NativeAction::ToRight,
        name: "vis-motion-to-right",
        handler: NativeHandler::MovementKey,
        argument: NativeArgument::Motion(NativeMotion::ToRight),
        help: "To the first occurrence of character to the right",
    },
    NativeActionDefinition {
        action: NativeAction::Nop,
        name: "vis-nop",
        handler: NativeHandler::Nop,
        argument: NativeArgument::None,
        help: "Ignore key, do nothing",
    },
    NativeActionDefinition {
        action: NativeAction::ModeNormalEscape,
        name: "vis-mode-normal-escape",
        handler: NativeHandler::NormalmodeEscape,
        argument: NativeArgument::None,
        help: "Reset count or remove all non-primary selections",
    },
    NativeActionDefinition {
        action: NativeAction::OpenLineAbove,
        name: "vis-open-line-above",
        handler: NativeHandler::Openline,
        argument: NativeArgument::Integer(-1),
        help: "Begin a new line above the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::OpenLineBelow,
        name: "vis-open-line-below",
        handler: NativeHandler::Openline,
        argument: NativeArgument::Integer(1),
        help: "Begin a new line below the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::OperatorChange,
        name: "vis-operator-change",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::Change),
        help: "Change operator",
    },
    NativeActionDefinition {
        action: NativeAction::OperatorDelete,
        name: "vis-operator-delete",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::Delete),
        help: "Delete operator",
    },
    NativeActionDefinition {
        action: NativeAction::OperatorShiftLeft,
        name: "vis-operator-shift-left",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::ShiftLeft),
        help: "Shift left operator",
    },
    NativeActionDefinition {
        action: NativeAction::OperatorShiftRight,
        name: "vis-operator-shift-right",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::ShiftRight),
        help: "Shift right operator",
    },
    NativeActionDefinition {
        action: NativeAction::OperatorYank,
        name: "vis-operator-yank",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::Yank),
        help: "Yank operator",
    },
    NativeActionDefinition {
        action: NativeAction::PutAfter,
        name: "vis-put-after",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::PutAfter),
        help: "Put text after the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::PutBefore,
        name: "vis-put-before",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::PutBefore),
        help: "Put text before the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLinesBegin,
        name: "vis-selection-new-lines-begin",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::CursorSol),
        help: "Create a new selection at the start of every line covered by selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLinesEnd,
        name: "vis-selection-new-lines-end",
        handler: NativeHandler::Operator,
        argument: NativeArgument::Operator(NativeOperator::CursorEol),
        help: "Create a new selection at the end of every line covered by selection",
    },
    NativeActionDefinition {
        action: NativeAction::CursorPercent,
        name: "vis-motion-percent",
        handler: NativeHandler::Percent,
        argument: NativeArgument::None,
        help: "Move to count % of file or matching item",
    },
    NativeActionDefinition {
        action: NativeAction::PromptSearchBackward,
        name: "vis-search-backward",
        handler: NativeHandler::PromptShow,
        argument: NativeArgument::Text("?"),
        help: "Search backward",
    },
    NativeActionDefinition {
        action: NativeAction::PromptSearchForward,
        name: "vis-search-forward",
        handler: NativeHandler::PromptShow,
        argument: NativeArgument::Text("/"),
        help: "Search forward",
    },
    NativeActionDefinition {
        action: NativeAction::PromptShow,
        name: "vis-prompt-show",
        handler: NativeHandler::PromptShow,
        argument: NativeArgument::Text(":"),
        help: "Show editor command line prompt",
    },
    NativeActionDefinition {
        action: NativeAction::Redo,
        name: "vis-redo",
        handler: NativeHandler::Redo,
        argument: NativeArgument::None,
        help: "Redo last change",
    },
    NativeActionDefinition {
        action: NativeAction::Register,
        name: "vis-register",
        handler: NativeHandler::Reg,
        argument: NativeArgument::None,
        help: "Use given register for next operator",
    },
    NativeActionDefinition {
        action: NativeAction::Repeat,
        name: "vis-repeat",
        handler: NativeHandler::Repeat,
        argument: NativeArgument::None,
        help: "Repeat latest editor command",
    },
    NativeActionDefinition {
        action: NativeAction::ReplaceChar,
        name: "vis-replace-char",
        handler: NativeHandler::Replace,
        argument: NativeArgument::None,
        help: "Replace the character under the cursor",
    },
    NativeActionDefinition {
        action: NativeAction::ModeReplace,
        name: "vis-mode-replace",
        handler: NativeHandler::Replacemode,
        argument: NativeArgument::Motion(NativeMotion::Nop),
        help: "Enter replace mode",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionFlip,
        name: "vis-selection-flip",
        handler: NativeHandler::SelectionEnd,
        argument: NativeArgument::None,
        help: "Flip selection, move cursor to other end",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsAlign,
        name: "vis-selections-align",
        handler: NativeHandler::SelectionsAlign,
        argument: NativeArgument::None,
        help: "Try to align all selections on the same column",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsAlignIndentLeft,
        name: "vis-selections-align-indent-left",
        handler: NativeHandler::SelectionsAlignIndent,
        argument: NativeArgument::Integer(-1),
        help: "Left-align all selections by inserting spaces",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsAlignIndentRight,
        name: "vis-selections-align-indent-right",
        handler: NativeHandler::SelectionsAlignIndent,
        argument: NativeArgument::Integer(1),
        help: "Right-align all selections by inserting spaces",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsCaseToupper,
        name: "vis-selections-case-toupper",
        handler: NativeHandler::SelectionsCase,
        argument: NativeArgument::Integer(1),
        help: "Uppercase all selections",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsCaseToggle,
        name: "vis-selections-case-toggle",
        handler: NativeHandler::SelectionsCase,
        argument: NativeArgument::Integer(0),
        help: "Toggle case of all selections",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsCaseTolower,
        name: "vis-selections-case-tolower",
        handler: NativeHandler::SelectionsCase,
        argument: NativeArgument::Integer(-1),
        help: "Lowercase all selections",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRemoveAll,
        name: "vis-selections-remove-all",
        handler: NativeHandler::SelectionsClear,
        argument: NativeArgument::None,
        help: "Remove all but the primary selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsComplement,
        name: "vis-selections-complement",
        handler: NativeHandler::SelectionsComplement,
        argument: NativeArgument::None,
        help: "Complement selections",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsIntersect,
        name: "vis-selections-intersect",
        handler: NativeHandler::SelectionsIntersect,
        argument: NativeArgument::None,
        help: "Intersect with selections from mark",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewMatchAll,
        name: "vis-selection-new-match-all",
        handler: NativeHandler::SelectionsMatchNext,
        argument: NativeArgument::Boolean(true),
        help: "Select all regions matching the current selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewMatchNext,
        name: "vis-selection-new-match-next",
        handler: NativeHandler::SelectionsMatchNext,
        argument: NativeArgument::None,
        help: "Select the next region matching the current selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewMatchSkip,
        name: "vis-selection-new-match-skip",
        handler: NativeHandler::SelectionsMatchSkip,
        argument: NativeArgument::None,
        help: "Clear current selection, but select next match",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsMinus,
        name: "vis-selections-minus",
        handler: NativeHandler::SelectionsMinus,
        argument: NativeArgument::None,
        help: "Subtract selections from mark",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNext,
        name: "vis-selection-next",
        handler: NativeHandler::SelectionsNavigate,
        argument: NativeArgument::Integer(2147483646),
        help: "Move to the next selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsPrev,
        name: "vis-selection-prev",
        handler: NativeHandler::SelectionsNavigate,
        argument: NativeArgument::Integer(-2147483646),
        help: "Move to the previous selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLineAbove,
        name: "vis-selection-new-lines-above",
        handler: NativeHandler::SelectionsNew,
        argument: NativeArgument::Integer(-1),
        help: "Create a new selection on the line above",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLineAboveFirst,
        name: "vis-selection-new-lines-above-first",
        handler: NativeHandler::SelectionsNew,
        argument: NativeArgument::Integer(-2147483648),
        help: "Create a new selection on the line above the first selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLineBelow,
        name: "vis-selection-new-lines-below",
        handler: NativeHandler::SelectionsNew,
        argument: NativeArgument::Integer(1),
        help: "Create a new selection on the line below",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsNewLineBelowLast,
        name: "vis-selection-new-lines-below-last",
        handler: NativeHandler::SelectionsNew,
        argument: NativeArgument::Integer(2147483647),
        help: "Create a new selection on the line below the last selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRemoveLast,
        name: "vis-selections-remove-last",
        handler: NativeHandler::SelectionsRemove,
        argument: NativeArgument::None,
        help: "Remove primary selection",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRemoveColumn,
        name: "vis-selections-remove-column",
        handler: NativeHandler::SelectionsRemoveColumn,
        argument: NativeArgument::Integer(1),
        help: "Remove count selection column",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRemoveColumnExcept,
        name: "vis-selections-remove-column-except",
        handler: NativeHandler::SelectionsRemoveColumnExcept,
        argument: NativeArgument::Integer(1),
        help: "Remove all but the count selection column",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRestore,
        name: "vis-selections-restore",
        handler: NativeHandler::SelectionsRestore,
        argument: NativeArgument::None,
        help: "Restore selections from mark",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRotateLeft,
        name: "vis-selections-rotate-left",
        handler: NativeHandler::SelectionsRotate,
        argument: NativeArgument::Integer(-1),
        help: "Rotate selections left",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsRotateRight,
        name: "vis-selections-rotate-right",
        handler: NativeHandler::SelectionsRotate,
        argument: NativeArgument::Integer(1),
        help: "Rotate selections right",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsSave,
        name: "vis-selections-save",
        handler: NativeHandler::SelectionsSave,
        argument: NativeArgument::None,
        help: "Save currently active selections to mark",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsTrim,
        name: "vis-selections-trim",
        handler: NativeHandler::SelectionsTrim,
        argument: NativeArgument::None,
        help: "Remove leading and trailing white space from selections",
    },
    NativeActionDefinition {
        action: NativeAction::SelectionsUnion,
        name: "vis-selections-union",
        handler: NativeHandler::SelectionsUnion,
        argument: NativeArgument::None,
        help: "Add selections from mark",
    },
    NativeActionDefinition {
        action: NativeAction::EditorSuspend,
        name: "vis-suspend",
        handler: NativeHandler::Suspend,
        argument: NativeArgument::None,
        help: "Suspend the editor",
    },
    NativeActionDefinition {
        action: NativeAction::ModeNormal,
        name: "vis-mode-normal",
        handler: NativeHandler::Switchmode,
        argument: NativeArgument::Mode(Mode::Normal),
        help: "Enter normal mode",
    },
    NativeActionDefinition {
        action: NativeAction::ModeVisual,
        name: "vis-mode-visual-charwise",
        handler: NativeHandler::Switchmode,
        argument: NativeArgument::Mode(Mode::Visual),
        help: "Enter characterwise visual mode",
    },
    NativeActionDefinition {
        action: NativeAction::ModeVisualLine,
        name: "vis-mode-visual-linewise",
        handler: NativeHandler::Switchmode,
        argument: NativeArgument::Mode(Mode::VisualLine),
        help: "Enter linewise visual mode",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectAngleBracketInner,
        name: "vis-textobject-angle-bracket-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerAngleBracket),
        help: "<> block (inner variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectAngleBracketOuter,
        name: "vis-textobject-angle-bracket-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterAngleBracket),
        help: "<> block (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectBacktickInner,
        name: "vis-textobject-backtick-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerBacktick),
        help: "A backtick delimited string (inner variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectBacktickOuter,
        name: "vis-textobject-backtick-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterBacktick),
        help: "A backtick delimited string (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectCurlyBracketInner,
        name: "vis-textobject-curly-bracket-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerCurlyBracket),
        help: "{} block (inner variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectCurlyBracketOuter,
        name: "vis-textobject-curly-bracket-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterCurlyBracket),
        help: "{} block (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectIndentation,
        name: "vis-textobject-indentation",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::Indentation),
        help: "All adjacent lines with the same indentation level as the current one",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectLineInner,
        name: "vis-textobject-line-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerLine),
        help: "The whole line, excluding leading and trailing whitespace",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectLineOuter,
        name: "vis-textobject-line-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterLine),
        help: "The whole line",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectLongwordInner,
        name: "vis-textobject-bigword-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerLongword),
        help: "A WORD leading and trailing whitespace excluded",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectLongwordOuter,
        name: "vis-textobject-bigword-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterLongword),
        help: "A WORD leading and trailing whitespace included",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectParagraph,
        name: "vis-textobject-paragraph",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::Paragraph),
        help: "A paragraph",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectParagraphOuter,
        name: "vis-textobject-paragraph-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::ParagraphOuter),
        help: "A paragraph (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectParenthesisInner,
        name: "vis-textobject-parenthesis-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerParenthesis),
        help: "() block (inner variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectParenthesisOuter,
        name: "vis-textobject-parenthesis-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterParenthesis),
        help: "() block (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectQuoteInner,
        name: "vis-textobject-quote-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerQuote),
        help: "A quoted string, excluding the quotation marks",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectQuoteOuter,
        name: "vis-textobject-quote-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterQuote),
        help: "A quoted string, including the quotation marks",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSearchBackward,
        name: "vis-textobject-search-backward",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::SearchBackward),
        help: "The next search match in backward direction",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSearchForward,
        name: "vis-textobject-search-forward",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::SearchForward),
        help: "The next search match in forward direction",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSentence,
        name: "vis-textobject-sentence",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::Sentence),
        help: "A sentence",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSingleQuoteInner,
        name: "vis-textobject-single-quote-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerSingleQuote),
        help: "A single quoted string, excluding the quotation marks",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSingleQuoteOuter,
        name: "vis-textobject-single-quote-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterSingleQuote),
        help: "A single quoted string, including the quotation marks",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSquareBracketInner,
        name: "vis-textobject-square-bracket-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerSquareBracket),
        help: "[] block (inner variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectSquareBracketOuter,
        name: "vis-textobject-square-bracket-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterSquareBracket),
        help: "[] block (outer variant)",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectWordInner,
        name: "vis-textobject-word-inner",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::InnerWord),
        help: "A word leading and trailing whitespace excluded",
    },
    NativeActionDefinition {
        action: NativeAction::TextObjectWordOuter,
        name: "vis-textobject-word-outer",
        handler: NativeHandler::TextObject,
        argument: NativeArgument::TextObject(NativeTextObject::OuterWord),
        help: "A word leading and trailing whitespace included",
    },
    NativeActionDefinition {
        action: NativeAction::Undo,
        name: "vis-undo",
        handler: NativeHandler::Undo,
        argument: NativeArgument::None,
        help: "Undo last change",
    },
    NativeActionDefinition {
        action: NativeAction::UnicodeInfo,
        name: "vis-unicode-info",
        handler: NativeHandler::UnicodeInfo,
        argument: NativeArgument::Info(NativeInfo::Unicode),
        help: "Show Unicode codepoint(s) of character under cursor",
    },
    NativeActionDefinition {
        action: NativeAction::Utf8Info,
        name: "vis-utf8-info",
        handler: NativeHandler::UnicodeInfo,
        argument: NativeArgument::Info(NativeInfo::Utf8),
        help: "Show UTF-8 encoded codepoint(s) of character under cursor",
    },
    NativeActionDefinition {
        action: NativeAction::ModeVisualEscape,
        name: "vis-mode-visual-escape",
        handler: NativeHandler::VisualmodeEscape,
        argument: NativeArgument::None,
        help: "Reset count or switch to normal mode",
    },
    NativeActionDefinition {
        action: NativeAction::WindowRedrawBottom,
        name: "vis-window-redraw-bottom",
        handler: NativeHandler::Window,
        argument: NativeArgument::Window(NativeWindow::Bottom),
        help: "Redraw cursor line at the bottom of the window",
    },
    NativeActionDefinition {
        action: NativeAction::WindowRedrawCenter,
        name: "vis-window-redraw-center",
        handler: NativeHandler::Window,
        argument: NativeArgument::Window(NativeWindow::Center),
        help: "Redraw cursor line at the center of the window",
    },
    NativeActionDefinition {
        action: NativeAction::WindowRedrawTop,
        name: "vis-window-redraw-top",
        handler: NativeHandler::Window,
        argument: NativeArgument::Window(NativeWindow::Top),
        help: "Redraw cursor line at the top of the window",
    },
    NativeActionDefinition {
        action: NativeAction::WindowHalfpageDown,
        name: "vis-window-halfpage-down",
        handler: NativeHandler::Wscroll,
        argument: NativeArgument::Scroll(NativeScroll::HalfPageDown),
        help: "Scroll window half pages forwards (downwards)",
    },
    NativeActionDefinition {
        action: NativeAction::WindowHalfpageUp,
        name: "vis-window-halfpage-up",
        handler: NativeHandler::Wscroll,
        argument: NativeArgument::Scroll(NativeScroll::HalfPageUp),
        help: "Scroll window half pages backwards (upwards)",
    },
    NativeActionDefinition {
        action: NativeAction::WindowPageDown,
        name: "vis-window-page-down",
        handler: NativeHandler::Wscroll,
        argument: NativeArgument::Scroll(NativeScroll::PageDown),
        help: "Scroll window pages forwards (downwards)",
    },
    NativeActionDefinition {
        action: NativeAction::WindowPageUp,
        name: "vis-window-page-up",
        handler: NativeHandler::Wscroll,
        argument: NativeArgument::Scroll(NativeScroll::PageUp),
        help: "Scroll window pages backwards (upwards)",
    },
    NativeActionDefinition {
        action: NativeAction::WindowSlideDown,
        name: "vis-window-slide-down",
        handler: NativeHandler::Wslide,
        argument: NativeArgument::Scroll(NativeScroll::SlideDown),
        help: "Slide window content downwards",
    },
    NativeActionDefinition {
        action: NativeAction::WindowSlideUp,
        name: "vis-window-slide-up",
        handler: NativeHandler::Wslide,
        argument: NativeArgument::Scroll(NativeScroll::SlideUp),
        help: "Slide window content upwards",
    },
];
impl NativeAction {
    pub fn definition(self) -> &'static NativeActionDefinition {
        &NATIVE_ACTIONS[self as usize]
    }
    pub fn name(self) -> &'static str {
        self.definition().name
    }
    pub fn from_name(name: &str) -> Option<Self> {
        NATIVE_ACTIONS
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.action)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn catalog_matches_every_c_action_name_and_help() {
        let source = include_str!("../../../main.c");
        let definitions: Vec<_> = source
            .split("#define ENUM")
            .next()
            .unwrap()
            .lines()
            .filter(|line| line.trim_start().starts_with("X("))
            .map(|line| {
                let strings: Vec<_> = line.split('"').collect();
                (strings[strings.len() - 4], strings[strings.len() - 2])
            })
            .collect();
        assert_eq!(definitions.len(), NATIVE_ACTIONS.len());
        let mut names = std::collections::BTreeSet::new();
        for ((name, help), definition) in definitions.into_iter().zip(NATIVE_ACTIONS) {
            assert!(names.insert(name));
            assert_eq!(definition.name, name);
            assert_eq!(definition.help, help);
            assert_eq!(NativeAction::from_name(name), Some(definition.action));
            assert_eq!(definition.action.definition(), definition);
        }
        assert_eq!(NativeAction::from_name("vis-not-an-action"), None);
    }
}

#[derive(Clone, Copy)]
struct MotionProperties {
    inclusive: bool,
    linewise: bool,
    idempotent: bool,
}
impl NativeMotion {
    fn properties(self) -> MotionProperties {
        match self {
            Self::BlockEnd => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::BlockStart => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::Byte => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::ByteLeft => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::ByteRight => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::CharNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::CharPrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::CodepointNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::CodepointPrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::Column => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::LineBegin => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::LineCharNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::LineCharPrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::LineDown => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: false,
            },
            Self::LineEnd => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::LineFinish => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: true,
            },
            Self::LineStart => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::LineUp => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: false,
            },
            Self::LongwordEndNext => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::LongwordEndPrev => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::LongwordStartNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::LongwordStartPrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::Nop => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: true,
            },
            Self::ParagraphNext => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: false,
            },
            Self::ParagraphPrev => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: false,
            },
            Self::ParenthesisEnd => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ParenthesisStart => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ScreenLineBegin => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ScreenLineDown => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ScreenLineEnd => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::ScreenLineMiddle => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ScreenLineUp => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchRepeat => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchRepeatBackward => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchRepeatForward => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchRepeatReverse => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchWordBackward => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SearchWordForward => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SentenceNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::SentencePrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::TillLeft => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::TillLineLeft => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::TillLineRight => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::TillRight => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::TotillRepeat => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::TotillReverse => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ToLeft => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ToLineLeft => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::ToLineRight => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::ToRight => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::WindowLineBottom => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: true,
            },
            Self::WindowLineMiddle => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: true,
            },
            Self::WindowLineTop => MotionProperties {
                inclusive: false,
                linewise: true,
                idempotent: true,
            },
            Self::WordEndNext => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::WordEndPrev => MotionProperties {
                inclusive: true,
                linewise: false,
                idempotent: false,
            },
            Self::WordStartNext => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
            Self::WordStartPrev => MotionProperties {
                inclusive: false,
                linewise: false,
                idempotent: false,
            },
        }
    }

    /// Native JUMP movement type: the resulting position is recorded in the
    /// per-window jump list after the motion completes.
    fn jumps(self) -> bool {
        matches!(
            self,
            Self::BlockEnd
                | Self::BlockStart
                | Self::ParagraphNext
                | Self::ParagraphPrev
                | Self::ParenthesisEnd
                | Self::ParenthesisStart
                | Self::SearchRepeat
                | Self::SearchRepeatBackward
                | Self::SearchRepeatForward
                | Self::SearchRepeatReverse
                | Self::SearchWordBackward
                | Self::SearchWordForward
                | Self::WindowLineBottom
                | Self::WindowLineMiddle
                | Self::WindowLineTop
        )
    }
}

/// Routing status is separate from recognition: every C name is recognized,
/// including actions whose underlying editor or viewport operation is missing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeSupport {
    Implemented,
    Conditional(&'static str),
    Unsupported(&'static str),
}
impl NativeAction {
    pub fn support(self) -> NativeSupport {
        use NativeArgument as A;
        use NativeHandler as H;
        use NativeMotion as M;
        match (self.definition().handler, self.definition().argument) {
            (
                H::Movement,
                A::Motion(
                    M::ScreenLineBegin
                    | M::ScreenLineDown
                    | M::ScreenLineEnd
                    | M::ScreenLineMiddle
                    | M::ScreenLineUp
                    | M::WindowLineBottom
                    | M::WindowLineMiddle
                    | M::WindowLineTop,
                ),
            ) => NativeSupport::Conditional("requires measured viewport geometry"),
            (H::Join, _) => NativeSupport::Conditional(
                "joining while another operator is pending is not implemented",
            ),
            (H::Operator, A::Operator(NativeOperator::CursorSol | NativeOperator::CursorEol)) => {
                NativeSupport::Conditional(
                    "cursor-creation operators currently require visual selections",
                )
            }
            (H::InsertRegister | H::InsertVerbatim, _) => {
                NativeSupport::Conditional("currently supported in insert and replace modes")
            }
            (
                H::Reg
                | H::Mark
                | H::MacroRecord
                | H::MacroReplay
                | H::Replace
                | H::MovementKey
                | H::TextObject,
                _,
            ) => NativeSupport::Conditional(
                "argument-taking actions currently require normal, visual, or operator mode",
            ),
            (H::Undo | H::Redo | H::Earlier | H::Later, _) => {
                NativeSupport::Conditional("native history actions currently require normal mode")
            }
            (H::Operator | H::Delete, _) => NativeSupport::Conditional(
                "native operators currently require normal, visual, or operator mode",
            ),
            (H::Call, A::Call(NativeCall::InsertNewline | NativeCall::InsertTab)) => {
                NativeSupport::Conditional(
                    "direct newline/tab actions do not yet support a pending operator",
                )
            }
            _ => NativeSupport::Implemented,
        }
    }
}

use super::{Editor, EditorAction, EditorError, Operator, Pending, Placement, Prompt, Register};
use crate::{
    motion,
    selection::Selection,
    view::{
        last_row_with_layout, last_source_row_with_layout, position_at_with_breakat,
        row_start_with_breakat, row_width_with_breakat, screen_position_with_layout,
    },
};

impl Editor {
    pub fn invoke_native_action(
        &mut self,
        action: NativeAction,
    ) -> Result<EditorAction, EditorError> {
        if let Some(callback) = self.actions.get(action.name()).copied() {
            return self.request_callback(
                super::CallbackKind::Mapping {
                    keys: self.input_queue(),
                },
                callback,
            );
        }
        if let NativeSupport::Unsupported(reason) = action.support() {
            return Err(Self::native_unsupported(action, reason));
        }
        let definition = action.definition();
        use NativeArgument as A;
        use NativeHandler as H;
        match (definition.handler, definition.argument) {
            (H::Movement, A::Motion(motion)) => {
                let jump = motion.jumps();
                let action = self.native_motion(motion)?;
                if jump {
                    self.jumplist_save();
                }
                if !matches!(motion, NativeMotion::LineUp | NativeMotion::LineDown) {
                    self.request_centered_follow();
                }
                return Ok(action);
            }
            (H::Window, A::Window(window)) => return self.native_window(window),
            (H::Wscroll | H::Wslide, A::Scroll(scroll)) => return self.native_scroll(scroll),
            (H::Call, A::Call(NativeCall::Redraw)) => return Ok(EditorAction::Continue),
            (H::Jumplist, A::Integer(advance)) => {
                let advance = isize::try_from(advance)
                    .map_err(|_| EditorError("jump list advance out of range".into()))?;
                return self.jumplist_navigate(advance);
            }
            (H::MovementKey, A::Motion(movement)) => {
                self.native_require_command_mode(action)?;
                if self.queued_keys.is_empty() {
                    self.language_map_disabled = true;
                }
                self.pending = Pending::Find {
                    backwards: matches!(
                        movement,
                        NativeMotion::ToLineLeft
                            | NativeMotion::TillLineLeft
                            | NativeMotion::ToLeft
                            | NativeMotion::TillLeft
                    ),
                    till: matches!(
                        movement,
                        NativeMotion::TillLineLeft
                            | NativeMotion::TillLineRight
                            | NativeMotion::TillLeft
                            | NativeMotion::TillRight
                    ),
                    cross_line: matches!(
                        movement,
                        NativeMotion::ToLeft
                            | NativeMotion::ToRight
                            | NativeMotion::TillLeft
                            | NativeMotion::TillRight
                    ),
                };
            }
            (H::Operator, A::Operator(operator)) => return self.native_operator(operator, action),
            (H::TextObject, A::TextObject(object)) => {
                self.native_require_command_mode(action)?;
                return self.native_text_object(object);
            }
            (H::Call, A::Call(NativeCall::InsertNewline)) => {
                self.repeatable &= matches!(self.mode, Mode::Insert | Mode::Replace);
                if self.operator.is_some() {
                    return Err(Self::native_unsupported(
                        action,
                        "direct newline with a pending operator is not implemented",
                    ));
                }
                self.insert_newline()?;
                self.last_insert_newline = false;
            }
            (H::Call, A::Call(NativeCall::InsertTab)) => {
                self.repeatable &= matches!(self.mode, Mode::Insert | Mode::Replace);
                if self.operator.is_some() {
                    return Err(Self::native_unsupported(
                        action,
                        "direct tab with a pending operator is not implemented",
                    ));
                }
                let data = self.text.to_vec();
                let edits = self
                    .selections()
                    .iter()
                    .map(|selection| {
                        let bytes = if self.expand_tab {
                            let width = self.tab_width.clamp(1, 8);
                            vec![b' '; width - self.display_column(&data, selection.cursor) % width]
                        } else {
                            vec![b'\t']
                        };
                        (selection.cursor..selection.cursor, bytes)
                    })
                    .collect();
                self.commit_multiple(edits, Placement::End)?;
            }
            (H::Call, A::Call(NativeCall::WindowNext)) => {
                return Ok(EditorAction::HostCommand("wincmd w".into()));
            }
            (H::Call, A::Call(NativeCall::WindowPrev)) => {
                return Ok(EditorAction::HostCommand("wincmd p".into()));
            }
            (H::Delete, A::Motion(movement)) => {
                self.native_operator(NativeOperator::Delete, action)?;
                return self.native_motion(movement);
            }
            (H::Gotoline, A::Integer(direction)) => {
                let data = self.text.to_vec();
                let explicit = self.count.is_some() || self.operator_count > 1;
                let count = self.combined_count()?;
                let target = if explicit {
                    motion::line_start(&data, Self::line_number(&data, count))
                } else if direction < 0 {
                    motion::line_start(&data, 0)
                } else {
                    motion::line_start(&data, data.len().saturating_sub(1))
                };
                self.motion_targets = self
                    .selections()
                    .iter()
                    .map(|source| Selection {
                        anchor: if self.is_visual() {
                            source.anchor
                        } else {
                            target
                        },
                        cursor: target,
                    })
                    .collect();
                let action = self.finish_motion(target, false, true)?;
                self.jumplist_save();
                self.request_centered_follow();
                return Ok(action);
            }
            (H::Insertmode | H::Replacemode, A::Motion(movement)) => {
                let count = self.combined_count()?;
                self.operator = None;
                self.operator_count = 1;
                let data = self.text.to_vec();
                let positions = self
                    .selections()
                    .iter()
                    .map(|selection| {
                        self.native_motion_position(&data, selection.cursor, movement, 1, None)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.text.snapshot();
                self.set_cursors(positions, self.primary_index())?;
                self.enter_insert(definition.handler == H::Replacemode, count);
            }
            (H::Openline, A::Integer(direction)) => {
                let count = self.combined_count()?;
                self.operator = None;
                self.operator_count = 1;
                self.text.snapshot();
                self.open_new_line(direction > 0)?;
                self.enter_insert(false, count);
                self.open_line = Some(direction > 0);
            }
            (H::Join, A::Text(separator)) => {
                if !matches!(self.mode, Mode::Normal | Mode::Visual | Mode::VisualLine) {
                    return Err(Self::native_unsupported(
                        action,
                        "join operator pending outside normal or visual mode is not implemented",
                    ));
                }
                return self.join_with_space(!separator.is_empty());
            }
            (H::Percent, _) => {
                if self.count.is_some() {
                    let ratio = self.take_count().min(100);
                    let target =
                        self.text.len() / 100 * ratio + self.text.len() % 100 * ratio / 100;
                    self.motion_targets = self
                        .selections()
                        .iter()
                        .map(|source| Selection {
                            anchor: if self.is_visual() {
                                source.anchor
                            } else {
                                target
                            },
                            cursor: target,
                        })
                        .collect();
                    return self.finish_motion(target, false, false);
                }
                let data = self.text.to_vec();
                self.motion_targets = self
                    .selections()
                    .iter()
                    .map(|source| {
                        let target =
                            motion::bracket_match(&data, source.cursor).unwrap_or(source.cursor);
                        Selection {
                            anchor: if self.is_visual() {
                                source.anchor
                            } else {
                                target
                            },
                            cursor: target,
                        }
                    })
                    .collect();
                return self.finish_motion(self.cursor, true, false);
            }
            (H::PromptShow, A::Text(title)) => {
                return self.show_prompt(match title {
                    "/" => Prompt::Search { backwards: false },
                    "?" => Prompt::Search { backwards: true },
                    _ => Prompt::Command,
                });
            }
            (H::Reg, _) => {
                self.native_require_command_mode(action)?;
                self.pending = Pending::Register;
            }
            (H::Mark, _) => {
                self.native_require_command_mode(action)?;
                self.pending = Pending::Mark;
            }
            (H::MacroRecord, _) => {
                self.native_require_command_mode(action)?;
                if let Some(name) = self.macro_record.take() {
                    self.macro_keys.clear();
                    self.last_macro = Some(name.to_ascii_lowercase());
                    self.command_keys.clear();
                } else {
                    self.pending = Pending::MacroRecord;
                }
            }
            (H::MacroReplay, _) => {
                self.native_require_command_mode(action)?;
                self.pending = Pending::MacroPlay;
            }
            (H::InsertRegister, _) => {
                self.native_require_input_mode(action)?;
                self.pending = Pending::InsertRegister;
            }
            (H::InsertVerbatim, _) => {
                self.native_require_input_mode(action)?;
                self.pending = Pending::VerbatimStart;
            }
            (H::Replace, _) => {
                self.native_require_command_mode(action)?;
                if self.queued_keys.is_empty() {
                    self.language_map_disabled = true;
                }
                self.pending = Pending::ReplaceChar;
            }
            (H::Repeat, _) => {
                let count = self.count.take();
                self.command_keys.clear();
                return self.repeat(count);
            }
            (H::Undo | H::Redo, _) => {
                if self.mode != Mode::Normal {
                    return Err(Self::native_unsupported(
                        action,
                        "native undo/redo outside normal mode is not implemented",
                    ));
                }
                let count = self.count;
                self.history_move(definition.handler == H::Redo, 1)?;
                self.count = count;
            }
            (H::Earlier | H::Later, _) => {
                if self.mode != Mode::Normal {
                    return Err(Self::native_unsupported(
                        action,
                        "native chronological history outside normal mode is not implemented",
                    ));
                }
                let count = self.take_count();
                let mut position = None;
                for _ in 0..count {
                    position = if definition.handler == H::Earlier {
                        self.text.earlier()
                    } else {
                        self.text.later()
                    };
                }
                if let Some(position) = position {
                    self.cursor = position.min(self.text.len());
                    self.sync_primary_to_set()?;
                }
            }
            (H::Suspend, _) => return Ok(EditorAction::HostCommand("suspend".into())),
            (H::Switchmode, A::Mode(mode)) => return self.native_switch_mode(mode),
            (H::NormalmodeEscape, _) => {
                if self.count.take().is_none() {
                    self.native_clear_selections()?;
                }
            }
            (H::VisualmodeEscape, _) => {
                if self.count.take().is_none() {
                    return self.native_switch_mode(Mode::Normal);
                }
            }
            (H::SelectionsCase, A::Integer(direction)) => {
                let fold = match direction.signum() {
                    -1 => super::CaseFold::Lower,
                    1 => super::CaseFold::Upper,
                    _ => super::CaseFold::Toggle,
                };
                return self.selection_case(fold);
            }
            (H::SelectionEnd, _) => {
                let data = self.text.to_vec();
                self.selection_set
                    .transform(&data, |selection| Selection {
                        anchor: selection.cursor,
                        cursor: selection.anchor,
                    })
                    .map_err(|e| EditorError(e.to_string()))?;
                self.sync_set_to_primary();
            }
            (H::SelectionsClear, _) => self.native_clear_selections()?,
            (H::SelectionsRemove, _) => {
                self.selection_set.remove_primary();
                self.sync_set_to_primary();
            }
            (H::SelectionsAlign, _) => self.align_cursors()?,
            (H::SelectionsAlignIndent, A::Integer(direction)) => {
                self.repeatable &= matches!(self.mode, Mode::Insert | Mode::Replace);
                self.align_indentation(direction > 0)?;
            }
            (H::SelectionsSave, _) => {
                let mark = std::mem::replace(&mut self.selected_mark, '\'');
                self.save_selection_mark(mark)?;
                self.count = None;
                self.operator = None;
            }
            (H::SelectionsRestore, _) => {
                let mark = std::mem::replace(&mut self.selected_mark, '\'');
                self.restore_selection_mark(mark, false)?;
                self.count = None;
                self.operator = None;
            }
            (
                H::SelectionsUnion
                | H::SelectionsIntersect
                | H::SelectionsMinus
                | H::SelectionsComplement,
                _,
            ) => {
                let data = self.text.to_vec();
                let mark = std::mem::replace(&mut self.selected_mark, '\'');
                let saved = self.saved_ranges(mark);
                match definition.handler {
                    H::SelectionsUnion => self.selection_set.union_with(&data, &saved),
                    H::SelectionsIntersect => self.selection_set.intersect_with(&data, &saved),
                    H::SelectionsMinus => self.selection_set.subtract(&data, &saved),
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
            (H::SelectionsTrim, _) => {
                if !self
                    .selection_set
                    .trim(&self.text.to_vec())
                    .map_err(|e| EditorError(e.to_string()))?
                {
                    self.mode = Mode::Normal;
                }
                self.sync_set_to_primary();
            }
            (H::SelectionsRotate, A::Integer(direction)) => {
                let count = self.take_count() as isize;
                self.selection_set
                    .rotate_contents(&mut self.text, if direction < 0 { -count } else { count })
                    .map_err(|e| EditorError(e.to_string()))?;
                self.repeatable = false;
                self.sync_set_to_primary();
            }
            (H::SelectionsNavigate, A::Integer(direction)) => {
                if self.selections().len() == 1 {
                    // Native `ka_selections_navigate` hands a single selection
                    // to the scroll action.
                    let scroll = if direction < 0 {
                        NativeScroll::ScrollUp
                    } else {
                        NativeScroll::ScrollDown
                    };
                    return self.native_scroll(scroll);
                }
                let count = self.take_count() as isize;
                self.selection_set
                    .navigate(if direction < 0 { -count } else { count });
                self.sync_set_to_primary();
            }
            (H::SelectionsRemoveColumn | H::SelectionsRemoveColumnExcept, _) => {
                let data = self.text.to_vec();
                let column = self.count.take().unwrap_or(1).saturating_sub(1);
                if self.selections().len() == 1 {
                    if definition.handler == H::SelectionsRemoveColumn {
                        self.cancel();
                    }
                } else {
                    if definition.handler == H::SelectionsRemoveColumn {
                        self.selection_set.remove_column(&data, column)
                    } else {
                        self.selection_set.keep_column(&data, column)
                    }
                    .map_err(|e| EditorError(e.to_string()))?;
                    self.sync_set_to_primary();
                }
            }
            (H::SelectionsMatchNext, argument) => {
                let before = self.selection_ids().to_vec();
                self.select_matching(matches!(argument, A::Boolean(true)), false)?;
                for id in self.selection_ids().to_vec() {
                    if !before.contains(&id) {
                        self.anchored.insert(id, true);
                    }
                }
            }
            (H::SelectionsMatchSkip, _) => {
                let before = self.selection_ids().to_vec();
                self.select_matching(false, true)?;
                for id in self.selection_ids().to_vec() {
                    if !before.contains(&id) {
                        self.anchored.insert(id, true);
                    }
                }
            }
            (H::SelectionsNew, A::Integer(direction)) => self.native_new_selection(direction)?,
            (H::UnicodeInfo, A::Info(info)) => {
                return Ok(EditorAction::Message(self.native_unicode_info(info)));
            }
            (H::Count | H::Nop, _) => {}
            _ => {
                return Err(Self::native_unsupported(
                    action,
                    "native handler is not routed yet",
                ));
            }
        }
        Ok(EditorAction::Continue)
    }

    fn native_unsupported(action: NativeAction, reason: &str) -> EditorError {
        EditorError(format!(
            "unsupported native action {}: {reason}",
            action.name()
        ))
    }
    fn native_require_command_mode(&self, action: NativeAction) -> Result<(), EditorError> {
        if matches!(self.mode, Mode::Insert | Mode::Replace | Mode::Command) {
            Err(Self::native_unsupported(
                action,
                "this argument-taking native action is not supported in input mode",
            ))
        } else {
            Ok(())
        }
    }
    fn native_require_input_mode(&self, action: NativeAction) -> Result<(), EditorError> {
        if matches!(self.mode, Mode::Insert | Mode::Replace) {
            Ok(())
        } else {
            Err(Self::native_unsupported(
                action,
                "this native input action requires insert or replace mode",
            ))
        }
    }

    fn native_operator(
        &mut self,
        operator: NativeOperator,
        action: NativeAction,
    ) -> Result<EditorAction, EditorError> {
        use NativeOperator as O;
        self.native_require_command_mode(action)?;
        match operator {
            O::PutAfter | O::PutBefore => return self.put(operator == O::PutAfter),
            O::CursorSol | O::CursorEol => {
                if !self.is_visual() {
                    return Err(Self::native_unsupported(
                        action,
                        "cursor-creation operator currently requires visual selections",
                    ));
                }
                return self.line_cursors(operator == O::CursorEol);
            }
            _ => {}
        }
        let operator = match operator {
            O::Delete => Operator::Delete,
            O::Change => Operator::Change,
            O::Yank => Operator::Yank,
            O::ShiftLeft => Operator::ShiftLeft,
            O::ShiftRight => Operator::ShiftRight,
            _ => unreachable!(),
        };
        let doubled = matches!(
            (self.operator, operator),
            (Some(Operator::Delete), Operator::Delete)
                | (Some(Operator::Change), Operator::Change)
                | (Some(Operator::Yank), Operator::Yank)
                | (Some(Operator::ShiftLeft), Operator::ShiftLeft)
                | (Some(Operator::ShiftRight), Operator::ShiftRight)
        );
        self.operator = Some(operator);
        if let Some(range) = self.selection() {
            return self.apply_operator(range, self.mode == Mode::VisualLine);
        }
        if doubled {
            let count = self.combined_count()?;
            let data = self.text.to_vec();
            let ranges = self
                .selections()
                .iter()
                .map(|s| {
                    let start = motion::line_begin(&data, s.cursor);
                    let mut end = start;
                    for _ in 0..count {
                        end = motion::line_next(&data, end);
                    }
                    start..end
                })
                .collect::<Vec<_>>();
            return if ranges.len() > 1 {
                self.apply_many_operators(ranges, true)
            } else {
                self.apply_operator(ranges[0].clone(), true)
            };
        }
        self.operator_count = 1;
        self.mode = Mode::OperatorPending;
        Ok(EditorAction::Continue)
    }

    fn native_clear_selections(&mut self) -> Result<(), EditorError> {
        if self.selections().len() > 1 {
            self.selection_set.keep_primary();
        } else {
            self.selection_set
                .collapse_all(&self.text.to_vec())
                .map_err(|e| EditorError(e.to_string()))?;
            let id = self.selection_ids()[0];
            self.anchored.insert(id, false);
        }
        self.sync_set_to_primary();
        Ok(())
    }
    pub(super) fn native_new_selection(&mut self, direction: i64) -> Result<(), EditorError> {
        let count = self.take_count();
        let anchored = self
            .selection_anchored(self.selection_set.primary_id())
            .unwrap_or(false);
        for _ in 0..count {
            let data = self.text.to_vec();
            let source = if direction == i64::from(i32::MIN) {
                self.selections()[0].cursor
            } else if direction == i64::from(i32::MAX) {
                self.selections().last().expect("selection set").cursor
            } else {
                self.cursor
            };
            let column = self.display_column(&data, source);
            let cursor = if direction < 0 {
                if motion::line_begin(&data, source) == 0 {
                    source
                } else {
                    self.column_position(
                        &data,
                        motion::line_begin(&data, motion::line_prev(&data, source)),
                        column,
                    )
                }
            } else {
                let next = motion::line_next(&data, source);
                if next >= data.len() {
                    source
                } else {
                    self.column_position(&data, next, column)
                }
            };
            let added = self
                .selection_set
                .insert_point_native(&data, cursor)
                .map_err(|e| EditorError(e.to_string()))?;
            if added.is_none() {
                let primary = self.primary_index();
                let target = if direction == -1 {
                    primary.checked_sub(1)
                } else if direction == 1 {
                    (primary + 1 < self.selections().len()).then_some(primary + 1)
                } else {
                    None
                };
                if let Some(target) = target {
                    self.selection_set
                        .set_primary(target)
                        .map_err(|e| EditorError(e.to_string()))?;
                }
            }
            let id = self.selection_set.primary_id();
            self.anchored.insert(id, anchored);
            self.sync_set_to_primary();
        }
        Ok(())
    }

    fn native_motion(&mut self, movement: NativeMotion) -> Result<EditorAction, EditorError> {
        use NativeMotion as M;
        let movement = match movement {
            M::SearchRepeat => {
                if self.search_backwards {
                    M::SearchRepeatBackward
                } else {
                    M::SearchRepeatForward
                }
            }
            M::SearchRepeatReverse => {
                if self.search_backwards {
                    M::SearchRepeatForward
                } else {
                    M::SearchRepeatBackward
                }
            }
            other => other,
        };
        if matches!(movement, M::TotillRepeat | M::TotillReverse) {
            let Some(mut find) = self.last_find else {
                return Ok(EditorAction::Continue);
            };
            if movement == M::TotillReverse {
                find.backwards = !find.backwards;
            }
            let count = self.combined_count()?;
            let (target, inclusive) = self.find_targets(find, count);
            return self.finish_motion(target, inclusive, false);
        }
        if matches!(
            movement,
            M::ScreenLineBegin
                | M::ScreenLineDown
                | M::ScreenLineEnd
                | M::ScreenLineMiddle
                | M::ScreenLineUp
                | M::WindowLineBottom
                | M::WindowLineMiddle
                | M::WindowLineTop
        ) {
            return self.native_view_motion(movement);
        }
        let explicit = self.count.is_some() || self.operator_count > 1;
        let count = self.combined_count()?;
        let raw = explicit.then_some(count);
        let data = self.text.to_vec();
        let origin = self.cursor;
        let mut targets = Vec::new();
        let properties = movement.properties();
        for selection in self.selections().to_vec() {
            let mut cursor = selection.cursor;
            if matches!(self.operator, Some(Operator::Change))
                && matches!(movement, M::WordStartNext | M::LongwordStartNext)
            {
                cursor = Self::native_change_word_target(
                    &data,
                    cursor,
                    count,
                    movement == M::LongwordStartNext,
                );
            } else if matches!(
                movement,
                M::SearchRepeatBackward
                    | M::SearchRepeatForward
                    | M::SearchWordBackward
                    | M::SearchWordForward
            ) {
                let backwards = matches!(movement, M::SearchRepeatBackward | M::SearchWordBackward);
                if matches!(movement, M::SearchWordBackward | M::SearchWordForward) {
                    let range = motion::word_object(&data, cursor, false, false);
                    let word = data[range].to_vec();
                    self.search = Self::escape_pattern(&word);
                    self.word_search = Some(word);
                    self.search_backwards = backwards;
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
                }
                self.cursor = cursor;
                for _ in 0..count {
                    if self.search_motion(backwards).is_err() {
                        self.cursor = cursor;
                        break;
                    }
                }
                cursor = self.cursor;
            } else if matches!(movement, M::LineUp | M::LineDown) {
                let column = self
                    .preferred_column
                    .unwrap_or_else(|| self.display_column(&data, cursor));
                self.preferred_column = Some(column);
                for _ in 0..count {
                    let start = if movement == M::LineUp {
                        if motion::line_begin(&data, cursor) == 0 {
                            break;
                        }
                        motion::line_begin(&data, motion::line_prev(&data, cursor))
                    } else {
                        let next = motion::line_next(&data, cursor);
                        if next >= data.len() {
                            break;
                        }
                        next
                    };
                    cursor = self.column_position(&data, start, column);
                }
            } else {
                let iterations = if properties.idempotent { 1 } else { count };
                for _ in 0..iterations {
                    let next = self.native_motion_position(&data, cursor, movement, count, raw)?;
                    if next == cursor {
                        break;
                    }
                    cursor = next;
                }
            }
            targets.push(Selection {
                anchor: if self.is_visual() {
                    selection.anchor
                } else {
                    cursor
                },
                cursor,
            });
        }
        self.cursor = origin;
        self.motion_targets = targets;
        self.finish_motion(origin, properties.inclusive, properties.linewise)
    }
    /// Native screen-line (`gj`-style) and window-line (`H`/`M`/`L`) motions.
    /// C keeps a per-view display line table; this port re-measures the wrapped
    /// layout with the body geometry the UI last reported, so no frame-sized
    /// state is retained. Window motions land on the start of a visible row;
    /// screen motions keep the preferred display column.
    /// Native `zt`/`zz`/`zb`: ask the next frame to place the cursor row at
    /// the top, center or bottom of the body. Cursor and text are unchanged.
    fn native_window(&mut self, window: NativeWindow) -> Result<EditorAction, EditorError> {
        use NativeWindow as W;
        let viewport = self.viewport;
        let data = self.text.to_vec();
        let (row, _) = screen_position_with_layout(
            &data,
            self.cursor,
            viewport.width.max(1),
            viewport.wrap_width(),
            self.tab_width,
            self.breakat(),
        );
        let top = match window {
            W::Top => row,
            W::Center => row.saturating_sub(viewport.height / 2),
            W::Bottom => row.saturating_sub(viewport.height.saturating_sub(1)),
        };
        self.request_scroll(top);
        Ok(EditorAction::Continue)
    }
    /// Native `view_scroll_page_up`, `view_scroll_page_down`,
    /// `view_scroll_halfpage_up`, `view_scroll_halfpage_down`,
    /// `view_slide_up`, `view_slide_down` and the plain `view_scroll_up` /
    /// `view_scroll_down`. The native view counts display lines, this port
    /// counts soft-wrapped rows; every rule below mirrors the native one.
    fn native_scroll(&mut self, scroll: NativeScroll) -> Result<EditorAction, EditorError> {
        let viewport = self.viewport;
        let physical_columns = viewport.width.max(1);
        let columns = viewport.wrap_width();
        let height = viewport.height.max(1);
        let tab_width = self.tab_width;
        let data = self.text.to_vec();
        let breakat = self.breakat.clone();
        let last = last_row_with_layout(&data, physical_columns, columns, tab_width, &breakat);
        let top = viewport.top.min(last);
        let (cursor_row, cursor_column) = screen_position_with_layout(
            &data,
            self.cursor,
            physical_columns,
            columns,
            tab_width,
            &breakat,
        );
        let row_begin = |row: usize| -> usize {
            row_start_with_breakat(&data, row, columns, tab_width, &breakat).unwrap_or(data.len())
        };
        let row_position = |row: usize, column: usize| -> usize {
            position_at_with_breakat(&data, row, column, columns, tab_width, &breakat)
                .unwrap_or(data.len())
        };
        // The view can only scroll while a row exists below the visible body.
        let can_scroll_down = top.saturating_add(height) <= last;
        let mut new_top = top;
        let mut cursor: Option<usize> = None;
        match scroll {
            NativeScroll::PageUp => {
                if top == 0 {
                    cursor = Some(0);
                } else {
                    // Native `view_redraw_bottom` scrolls a page up and keeps
                    // the row above the old top at the bottom of the page.
                    let previous = top - 1;
                    new_top = top.saturating_sub(height);
                    while previous >= new_top.saturating_add(height) {
                        new_top += 1;
                    }
                    cursor = Some(row_begin(previous));
                }
            }
            NativeScroll::PageDown => {
                if can_scroll_down {
                    new_top = top + height;
                    cursor = Some(row_begin(new_top));
                } else {
                    cursor = Some(data.len());
                }
            }
            NativeScroll::HalfPageUp => {
                if top == 0 {
                    cursor = Some(0);
                } else {
                    // Native `view_redraw_center` centers the row above the
                    // old top and keeps it selected.
                    let previous = top - 1;
                    new_top = previous.saturating_sub(height / 2);
                    let index = previous - new_top;
                    if index < height / 2 {
                        new_top = new_top.saturating_sub(height / 2 - index);
                    }
                    if previous < new_top {
                        new_top = previous;
                    }
                    cursor = Some(row_begin(previous));
                }
            }
            NativeScroll::HalfPageDown => {
                if can_scroll_down {
                    let end_row = top + height;
                    new_top = top + height / 2;
                    // Native `view_scroll_halfpage_down` selects the byte
                    // where the old view ended; the drawn newline cell makes
                    // that the first byte of the following row.
                    cursor = Some(if end_row <= last {
                        row_begin(end_row)
                    } else {
                        row_begin(new_top)
                    });
                } else {
                    cursor = Some(data.len());
                }
            }
            NativeScroll::SlideUp => {
                let count = self.combined_count()?.max(1);
                if can_scroll_down {
                    new_top = top + count.min(height);
                    if cursor_row == top {
                        // The cursor sat on the first visible row and follows
                        // the content.
                        cursor = Some(row_position(new_top, cursor_column));
                    }
                } else if cursor_row + 1 < top + height {
                    // Without content below, native slides the cursor down.
                    cursor = Some(row_position(cursor_row + 1, cursor_column));
                }
            }
            NativeScroll::SlideDown => {
                let count = self.combined_count()?.max(1);
                if top > 0 {
                    new_top = top.saturating_sub(count);
                    if cursor_row + 1 == top + height {
                        // The cursor sat on the last visible row and follows
                        // the content.
                        let row = (new_top + height - 1).min(last);
                        cursor = Some(row_position(row, cursor_column));
                    }
                } else if cursor_row > 0 {
                    // At the top, native slides the cursor up.
                    cursor = Some(row_position(cursor_row - 1, cursor_column));
                }
            }
            NativeScroll::ScrollUp => {
                let count = self.combined_count()?.max(1);
                if top > 0 {
                    new_top = top.saturating_sub(count);
                    if cursor_row + 1 == top + height {
                        let row = (new_top + height - 1).min(last);
                        cursor = Some(row_position(row, cursor_column));
                    }
                } else {
                    cursor = Some(0);
                }
            }
            NativeScroll::ScrollDown => {
                let count = self.combined_count()?.max(1);
                if can_scroll_down {
                    new_top = top + count.min(height);
                    if cursor_row < new_top {
                        cursor = Some(row_begin(new_top));
                    }
                } else {
                    cursor = Some(data.len());
                }
            }
        }
        if let Some(cursor) = cursor {
            self.set_primary_cursor(cursor)?;
        }
        self.request_scroll(new_top);
        Ok(EditorAction::Continue)
    }
    /// Move the primary cursor without touching the other selections. Native
    /// `cursor_set` keeps the anchor of a visual selection.
    fn set_primary_cursor(&mut self, cursor: usize) -> Result<(), EditorError> {
        let data = self.text.to_vec();
        let cursor = cursor.min(data.len());
        let selection = Selection {
            anchor: if self.is_visual() {
                self.anchor
            } else {
                cursor
            },
            cursor,
        };
        if cursor == data.len() && self.is_visual() {
            // Native `view_cursors_to` stores an end-of-text cursor verbatim;
            // deriving the selection range would round it to the last byte.
            let id = self.selection_set.primary_id();
            self.selection_set
                .update(&data, id, selection)
                .map_err(|e| EditorError(e.to_string()))?;
            self.sync_set_to_primary();
            return Ok(());
        }
        self.selection_set
            .replace_primary(&data, selection)
            .map_err(|e| EditorError(e.to_string()))?;
        self.sync_set_to_primary();
        Ok(())
    }
    fn native_view_motion(&mut self, movement: NativeMotion) -> Result<EditorAction, EditorError> {
        use NativeMotion as M;
        let viewport = self.viewport;
        let explicit = self.count.is_some() || self.operator_count > 1;
        let count = self.combined_count()?;
        // Native window motions default their count to 0 (`L` measures from
        // the bottom) unless the user supplied one.
        let bottom_count = if explicit { count } else { 0 };
        let data = self.text.to_vec();
        let physical_columns = viewport.width.max(1);
        let columns = viewport.wrap_width();
        let tab_width = self.tab_width;
        let last =
            last_row_with_layout(&data, physical_columns, columns, tab_width, self.breakat());
        let locate = |row: usize, column: usize| -> Option<usize> {
            if row > last {
                return None;
            }
            if row < last {
                return position_at_with_breakat(
                    &data,
                    row,
                    column,
                    columns,
                    tab_width,
                    self.breakat(),
                );
            }
            // The final row may hold only the EOF cursor cell.
            Some(
                position_at_with_breakat(&data, row, column, columns, tab_width, self.breakat())
                    .unwrap_or(data.len()),
            )
        };
        let source_last = last_source_row_with_layout(
            &data,
            physical_columns,
            columns,
            tab_width,
            self.breakat(),
        );
        let locate_row = |row: usize| -> Option<usize> {
            match source_last {
                Some(source_last) => {
                    let row = row.min(source_last);
                    row_start_with_breakat(&data, row, columns, tab_width, self.breakat())
                }
                None => Some(data.len()),
            }
        };
        let origin = self.cursor;
        let properties = movement.properties();
        let mut targets = Vec::new();
        let mut preferred = self.preferred_column;
        for selection in self.selections().to_vec() {
            let cursor = selection.cursor.min(data.len());
            let (row, column) = screen_position_with_layout(
                &data,
                cursor,
                physical_columns,
                columns,
                tab_width,
                self.breakat(),
            );
            let desired = preferred.unwrap_or(column);
            let target = match movement {
                M::ScreenLineBegin => locate(row, 0),
                M::ScreenLineMiddle => {
                    row_width_with_breakat(&data, row, columns, tab_width, self.breakat())
                        .and_then(|width| locate(row, width / 2))
                }
                M::ScreenLineEnd => {
                    row_width_with_breakat(&data, row, columns, tab_width, self.breakat())
                        .and_then(|width| locate(row, width.saturating_sub(1)))
                }
                M::ScreenLineUp => {
                    preferred = Some(desired);
                    locate(row.saturating_sub(count), desired)
                }
                M::ScreenLineDown => {
                    preferred = Some(desired);
                    if row >= last {
                        // Past the last display row native screen-line-down
                        // falls back to the line below, which is EOF here.
                        Some(data.len())
                    } else {
                        locate(row.saturating_add(count).min(last), desired)
                    }
                }
                M::WindowLineTop => {
                    locate_row(viewport.top.saturating_add(count.max(1) - 1).min(last))
                }
                M::WindowLineMiddle => locate_row(
                    viewport
                        .top
                        .saturating_add(viewport.height / 2)
                        .saturating_sub(1)
                        .min(last),
                ),
                M::WindowLineBottom => {
                    let offset = viewport.height.saturating_sub(bottom_count);
                    locate_row(
                        viewport
                            .top
                            .saturating_add(offset.saturating_sub(1))
                            .min(last),
                    )
                }
                _ => None,
            }
            .unwrap_or(cursor);
            targets.push(Selection {
                anchor: if self.is_visual() {
                    selection.anchor
                } else {
                    target
                },
                cursor: target,
            });
        }
        self.preferred_column = preferred;
        if self.operator.is_some() && properties.linewise {
            // Native window motions are LINEWISE without LINEWISE_INCLUSIVE:
            // the destination line joins the range only when the destination is
            // not already a line start, so `dH` on a line start is empty and
            // `dL` stops before the bottom row's line.
            let data = self.text.to_vec();
            let ranges: Vec<_> = self
                .selections()
                .iter()
                .zip(&targets)
                .map(|(source, target)| {
                    let start = source.cursor.min(target.cursor);
                    let end = source.cursor.max(target.cursor);
                    let line_start = motion::line_begin(&data, start);
                    let line_end = if end != motion::line_begin(&data, end) {
                        motion::line_next(&data, end)
                    } else {
                        end
                    };
                    line_start..line_end
                })
                .collect();
            self.motion_targets.clear();
            return if ranges.len() > 1 {
                self.apply_many_operators(ranges, true)
            } else {
                self.apply_operator(ranges[0].clone(), true)
            };
        }
        self.motion_targets = targets;
        self.finish_motion(origin, properties.inclusive, properties.linewise)
    }
    fn native_change_word_target(
        data: &[u8],
        mut position: usize,
        mut count: usize,
        big: bool,
    ) -> usize {
        let Some(&current) = data.get(position) else {
            return position;
        };
        let boundary = |byte: u8| {
            if big {
                byte.is_ascii_whitespace()
            } else {
                byte.is_ascii() && !(byte.is_ascii_alphanumeric() || byte == b'_')
            }
        };
        type TextMotion = fn(&[u8], usize) -> usize;
        let (movement, inclusive): (TextMotion, bool) = if current.is_ascii_whitespace() {
            (
                if big {
                    motion::longword_start_next
                } else {
                    motion::word_start_next
                },
                false,
            )
        } else if !boundary(current)
            && data
                .get(motion::char_next(data, position))
                .is_some_and(|&next| boundary(next))
        {
            if count == 1 {
                (motion::char_next, false)
            } else {
                count -= 1;
                (
                    if big {
                        motion::longword_end_next
                    } else {
                        motion::word_end_next
                    },
                    true,
                )
            }
        } else {
            (
                if big {
                    motion::longword_end_next
                } else {
                    motion::word_end_next
                },
                true,
            )
        };
        for _ in 0..count {
            let next = movement(data, position);
            if next == position {
                break;
            }
            position = next;
        }
        if inclusive {
            motion::char_next(data, position)
        } else {
            position
        }
    }
    fn native_motion_position(
        &self,
        data: &[u8],
        position: usize,
        movement: NativeMotion,
        count: usize,
        raw: Option<usize>,
    ) -> Result<usize, EditorError> {
        use NativeMotion as M;
        Ok(match movement {
            M::BlockStart => motion::block_start(data, position),
            M::BlockEnd => motion::block_end(data, position),
            M::ParenthesisStart => motion::parenthesis_start(data, position),
            M::ParenthesisEnd => motion::parenthesis_end(data, position),
            M::Byte => raw.unwrap_or(0).min(data.len()),
            M::ByteLeft => position.saturating_sub(count),
            M::ByteRight => position.saturating_add(count).min(data.len()),
            M::CharNext => motion::char_next(data, position),
            M::CharPrev => motion::char_prev(data, position),
            M::CodepointNext => motion::codepoint_next(data, position),
            M::CodepointPrev => motion::codepoint_prev(data, position),
            M::Column => motion::line_offset(data, position, raw.unwrap_or(0)),
            M::LineBegin => motion::line_begin(data, position),
            M::LineStart => motion::line_start(data, position),
            M::LineEnd => motion::line_end(data, position),
            M::LineFinish => motion::line_finish(data, position),
            M::LineCharNext => motion::line_char_next(data, position),
            M::LineCharPrev => motion::line_char_prev(data, position),
            M::LineUp => motion::line_up(data, position),
            M::LineDown => motion::line_down(data, position),
            M::WordStartNext => motion::word_start_next(data, position),
            M::WordStartPrev => motion::word_start_prev(data, position),
            M::WordEndNext => motion::word_end_next(data, position),
            M::WordEndPrev => motion::word_end_prev(data, position),
            M::LongwordStartNext => motion::longword_start_next(data, position),
            M::LongwordStartPrev => motion::longword_start_prev(data, position),
            M::LongwordEndNext => motion::longword_end_next(data, position),
            M::LongwordEndPrev => motion::longword_end_prev(data, position),
            M::ParagraphNext => motion::paragraph_next(data, position),
            M::ParagraphPrev => motion::paragraph_prev(data, position),
            M::SentenceNext => motion::sentence_next(data, position),
            M::SentencePrev => motion::sentence_prev(data, position),
            M::Nop => position,
            _ => {
                return Err(EditorError(
                    "unsupported native motion requires a specialized editor or viewport context"
                        .into(),
                ));
            }
        })
    }
    fn native_text_object(
        &mut self,
        object: NativeTextObject,
    ) -> Result<EditorAction, EditorError> {
        use NativeTextObject as O;
        let (character, outer) = match object {
            O::SearchBackward => return self.search_object(true),
            O::SearchForward => return self.search_object(false),
            O::InnerWord => ('w', false),
            O::OuterWord => ('w', true),
            O::InnerLongword => ('W', false),
            O::OuterLongword => ('W', true),
            O::InnerLine => ('l', false),
            O::OuterLine => ('l', true),
            O::Paragraph => ('p', false),
            O::ParagraphOuter => ('p', true),
            O::Sentence => ('s', false),
            O::Indentation => ('\t', false),
            O::InnerAngleBracket => ('<', false),
            O::OuterAngleBracket => ('<', true),
            O::InnerBacktick => ('`', false),
            O::OuterBacktick => ('`', true),
            O::InnerCurlyBracket => ('{', false),
            O::OuterCurlyBracket => ('{', true),
            O::InnerParenthesis => ('(', false),
            O::OuterParenthesis => ('(', true),
            O::InnerQuote => ('"', false),
            O::OuterQuote => ('"', true),
            O::InnerSingleQuote => ('\'', false),
            O::OuterSingleQuote => ('\'', true),
            O::InnerSquareBracket => ('[', false),
            O::OuterSquareBracket => ('[', true),
        };
        self.text_object(character, outer)
    }
    fn native_switch_mode(&mut self, mode: Mode) -> Result<EditorAction, EditorError> {
        if mode == self.mode {
            return Ok(EditorAction::Continue);
        }
        if mode == Mode::Normal && matches!(self.mode, Mode::Insert | Mode::Replace) {
            let newline = std::mem::replace(&mut self.last_insert_newline, false);
            return self.leave_insert_mode(newline);
        }
        let old_visual = self.is_visual();
        if old_visual && !matches!(mode, Mode::Visual | Mode::VisualLine) && self.operator.is_none()
        {
            self.save_selection_mark('^')?;
            self.jumplist_save();
        }
        self.mode = mode;
        let visual = self.is_visual();
        if !old_visual && visual {
            self.anchor = self.selection_set.primary().anchor;
            for id in self.selection_ids().to_vec() {
                self.anchored.insert(id, true);
            }
        }
        if old_visual && !visual {
            self.selection_set
                .collapse_all(&self.text.to_vec())
                .map_err(|e| EditorError(e.to_string()))?;
            for id in self.selection_ids().to_vec() {
                self.anchored.insert(id, false);
            }
            self.sync_set_to_primary();
        }
        if mode == Mode::VisualLine && self.operator.is_none() {
            self.materialize_visual_lines()?;
            self.count = None;
        }
        Ok(EditorAction::Continue)
    }
    fn native_unicode_info(&self, info: NativeInfo) -> String {
        let data = self.text.to_vec();
        let end = motion::char_next(&data, self.cursor);
        let mut position = self.cursor;
        let mut message = String::new();
        while position < end {
            let next = motion::codepoint_next(&data, position);
            let bytes = &data[position..next];
            let character = std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{fffd}');
            if bytes[0] < 128 && bytes[0].is_ascii_control() {
                let control = if bytes[0] == 127 {
                    '?'
                } else {
                    char::from(bytes[0] + 64)
                };
                message.push_str(&format!("<^{control}> "));
            } else {
                let prefix = if motion::codepoint_width(&data, position) == 0 {
                    " "
                } else {
                    ""
                };
                message.push_str(&format!("<{prefix}{}> ", String::from_utf8_lossy(bytes)));
            }
            if info == NativeInfo::Unicode {
                message.push_str(&format!("U+{:04X} ", character as u32));
            } else {
                for byte in bytes {
                    message.push_str(&format!("{byte:02x} "));
                }
            }
            position = next;
        }
        message
    }
}

#[cfg(test)]
mod argument_contract_tests {
    use super::*;
    fn snake(value: impl std::fmt::Debug) -> String {
        let value = format!("{value:?}");
        let mut result = String::new();
        for (index, character) in value.chars().enumerate() {
            if character.is_ascii_uppercase() && index > 0 {
                result.push('_');
            }
            result.push(character.to_ascii_uppercase());
        }
        result
    }
    #[test]
    fn typed_handlers_and_arguments_match_c_contract() {
        let rows = include_str!("../../../main.c")
            .split("#define ENUM")
            .next()
            .unwrap()
            .lines()
            .filter(|line| line.trim_start().starts_with("X("));
        for (row, definition) in rows.zip(NATIVE_ACTIONS) {
            let fields: Vec<_> = row
                .trim()
                .trim_start_matches("X(")
                .splitn(4, ',')
                .map(str::trim)
                .collect();
            assert_eq!(
                fields[0],
                format!("ka_{}", snake(definition.handler).to_ascii_lowercase())
            );
            let argument = fields[2];
            let expected = match definition.argument {
                NativeArgument::None => "0".into(),
                NativeArgument::Motion(value) => format!(".i = VIS_MOVE_{}", snake(value)),
                NativeArgument::Operator(value) => format!(".i = VIS_OP_{}", snake(value)),
                NativeArgument::TextObject(value) => {
                    format!(".i = VIS_TEXTOBJECT_{}", snake(value))
                }
                NativeArgument::Mode(value) => format!(".i = VIS_MODE_{}", snake(value)),
                NativeArgument::Text(value) => format!(".s = \"{value}\""),
                NativeArgument::Boolean(value) => format!(".b = {value}"),
                NativeArgument::Call(value) => format!(
                    ".f = {}",
                    match value {
                        NativeCall::InsertNewline => "vis_insert_nl",
                        NativeCall::InsertTab => "vis_insert_tab",
                        NativeCall::Redraw => "vis_redraw",
                        NativeCall::WindowNext => "vis_window_next",
                        NativeCall::WindowPrev => "vis_window_prev",
                    }
                ),
                NativeArgument::Window(value) => {
                    format!(".w = view_redraw_{}", snake(value).to_ascii_lowercase())
                }
                NativeArgument::Scroll(value) => match value {
                    NativeScroll::PageDown => ".i = +PAGE".into(),
                    NativeScroll::PageUp => ".i = -PAGE".into(),
                    NativeScroll::HalfPageDown => ".i = +PAGE_HALF".into(),
                    NativeScroll::HalfPageUp => ".i = -PAGE_HALF".into(),
                    NativeScroll::SlideDown | NativeScroll::ScrollDown => ".i = +1".into(),
                    NativeScroll::SlideUp | NativeScroll::ScrollUp => ".i = -1".into(),
                },
                NativeArgument::Info(value) => format!(
                    ".i = VIS_ACTION_{}_INFO",
                    if value == NativeInfo::Utf8 {
                        "UTF8"
                    } else {
                        "UNICODE"
                    }
                ),
                NativeArgument::Integer(value) => {
                    let text = argument.strip_prefix(".i = ").unwrap();
                    let parsed = match text {
                        "INT_MIN" => i64::from(i32::MIN),
                        "INT_MAX" | "+PAGE" => i64::from(i32::MAX),
                        "-PAGE" => -i64::from(i32::MAX),
                        "+PAGE_HALF" => i64::from(i32::MAX) - 1,
                        "-PAGE_HALF" => -i64::from(i32::MAX) + 1,
                        _ => text.parse().unwrap(),
                    };
                    assert_eq!(value, parsed, "{}", definition.name);
                    continue;
                }
            };
            assert_eq!(argument, expected, "{}", definition.name);
        }
    }
}

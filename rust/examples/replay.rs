//! Headless modal replay: final document bytes go to stdout, never back to input.
//! `cargo run --example replay -- INPUT KEYS [--state PATH]`
//!
//! Physical key-file newlines and trailing horizontal whitespace are formatting.
//! Use <Enter>, <Space>, and <Tab> for those keys. C preprocessor comments in
//! upstream Vim fixtures must be expanded by the differential runner first.

use std::error::Error;
use std::fmt;
use std::io::{self, Write};
use std::path::PathBuf;
use vis::editor::{Editor, EditorAction, Key, Mode};
use vis::text::Text;

const MAX_KEYS: usize = 1_000_000;

#[derive(Debug)]
struct ReplayError {
    message: String,
    unsupported: bool,
}

impl ReplayError {
    fn ordinary(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unsupported: false,
        }
    }

    fn editor(index: usize, key: Key, message: String) -> Self {
        let unsupported = message.contains("unsupported")
            || message.contains("not implemented")
            || message.contains("not supported")
            || message.contains("unknown text object");
        Self {
            message: format!("key {index} {key:?}: {message}"),
            unsupported,
        }
    }
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl Error for ReplayError {}

fn token(name: &str) -> Result<Option<Key>, ReplayError> {
    let lower = name.to_ascii_lowercase();
    let key = match lower.as_str() {
        "escape" | "esc" => Key::Escape,
        "enter" | "return" | "cr" => Key::Enter,
        "space" => Key::Char(' '),
        "tab" => Key::Tab,
        "s-tab" | "backtab" => Key::BackTab,
        "backspace" | "bs" | "c-h" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "left" => Key::Left,
        "right" => Key::Right,
        "up" => Key::Up,
        "down" => Key::Down,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "page-up" => Key::PageUp,
        "pagedown" | "page-down" => Key::PageDown,
        "insert" | "ins" => Key::Insert,
        "s-left" => Key::ShiftLeft,
        "s-right" => Key::ShiftRight,
        "s-pageup" | "s-page-up" => Key::ShiftPageUp,
        "s-pagedown" | "s-page-down" => Key::ShiftPageDown,
        "lt" => Key::Char('<'),
        _ => {
            if let Some(number) = lower.strip_prefix('f').filter(|number| {
                !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
            }) {
                if let Ok(number) = number.parse::<u8>() {
                    if number > 0 {
                        return Ok(Some(Key::Function(number)));
                    }
                }
                return Err(ReplayError {
                    message: format!("unsupported key token <{name}>"),
                    unsupported: true,
                });
            }
            if let Some(alt) = name
                .get(..2)
                .filter(|prefix| {
                    prefix.eq_ignore_ascii_case("m-") || prefix.eq_ignore_ascii_case("a-")
                })
                .and_then(|_| name.get(2..))
            {
                let control = alt
                    .get(..2)
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("c-"));
                let value = if control { &alt[2..] } else { alt };
                let character = match value.to_ascii_lowercase().as_str() {
                    "space" => Some(' '),
                    "tab" if control => Some('i'),
                    _ => {
                        let mut chars = value.chars();
                        match (chars.next(), chars.next()) {
                            (Some(c), None) => Some(c),
                            _ => None,
                        }
                    }
                };
                if let Some(character) = character {
                    if !control || character.is_ascii() {
                        return Ok(Some(if control {
                            Key::AltCtrl(character.to_ascii_lowercase())
                        } else {
                            Key::Alt(character)
                        }));
                    }
                }
                return Err(ReplayError {
                    message: format!("unsupported key token <{name}>"),
                    unsupported: true,
                });
            }
            if let Some(control) = lower.strip_prefix("c-") {
                let mut chars = control.chars();
                if let (Some(c), None) = (chars.next(), chars.next()) {
                    if c.is_ascii() {
                        return Ok(Some(Key::Ctrl(c)));
                    }
                }
                return Err(ReplayError {
                    message: format!("unsupported key token <{name}>"),
                    unsupported: true,
                });
            }
            if lower.starts_with("a-")
                || lower.starts_with("m-")
                || lower.starts_with("s-")
                || lower.starts_with("page")
                || lower == "insert"
                || lower
                    .strip_prefix('f')
                    .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            {
                return Err(ReplayError {
                    message: format!("unsupported key token <{name}>"),
                    unsupported: true,
                });
            }
            // Literal source may contain angle brackets, for example f<i<Space>.
            // Like test/util/keys.c, unrecognized non-key text stays literal.
            return Ok(None);
        }
    };
    Ok(Some(key))
}

fn keys(source: &str) -> Result<Vec<Key>, ReplayError> {
    let source: String = source
        .lines()
        .map(|line| line.trim_end_matches([' ', '\t', '\r']))
        .collect();
    let mut remaining = source.as_str();
    let mut keys = Vec::new();
    while !remaining.is_empty() {
        if keys.len() >= MAX_KEYS {
            return Err(ReplayError::ordinary("key replay limit exceeded"));
        }
        if remaining.starts_with('<') {
            if let Some(end) = remaining.find('>') {
                if let Some(key) = token(&remaining[1..end])? {
                    keys.push(key);
                    remaining = &remaining[end + 1..];
                    continue;
                }
            }
        }
        let c = remaining
            .chars()
            .next()
            .expect("remaining source is nonempty");
        keys.push(match c {
            '\u{1b}' => Key::Escape,
            '\u{8}' | '\u{7f}' => Key::Backspace,
            '\t' => Key::Tab,
            c if c.is_ascii_control() => {
                Key::Ctrl(char::from((c as u8) + b'@').to_ascii_lowercase())
            }
            c => Key::Char(c),
        });
        remaining = &remaining[c.len_utf8()..];
    }
    Ok(keys)
}

fn apply(editor: &mut Editor, key: Key, index: usize) -> Result<bool, ReplayError> {
    let action = match editor.handle_key(key) {
        Ok(action) => action,
        Err(error) => {
            let error = ReplayError::editor(index, key, error.to_string());
            if error.unsupported {
                return Err(error);
            }
            // The interactive host displays operational errors and accepts the
            // next key. Differential checks still compare the resulting bytes
            // and full selection/mode state after these failed operations.
            eprintln!("replay: {error}");
            return Ok(false);
        }
    };
    match action {
        EditorAction::Continue => Ok(false),
        EditorAction::PromptShow { .. }
        | EditorAction::PromptSubmit
        | EditorAction::PromptCancel
        | EditorAction::PromptHistoryExpand => Err(ReplayError {
            message: "hosted prompts require a workspace".into(),
            unsupported: true,
        }),
        EditorAction::Input(_) => Err(ReplayError {
            message: "input interception requires the Lua runtime".into(),
            unsupported: true,
        }),
        EditorAction::Callback(_) => Err(ReplayError {
            message: "callback requires the Lua runtime".into(),
            unsupported: true,
        }),
        EditorAction::Message(_) => Ok(false),
        EditorAction::HostCommand(command) => Err(ReplayError {
            message: format!("host command is not supported by headless replay: {command}"),
            unsupported: true,
        }),
        EditorAction::Quit { force } => {
            if editor.text.modified() && !force {
                eprintln!("replay: quit rejected: unsaved changes");
                return Ok(false);
            }
            Ok(true)
        }
        EditorAction::Save { quit, .. } => {
            // Simulate successful save acknowledgement; do not open any path.
            editor.text.mark_saved();
            Ok(quit)
        }
    }
}

/// Preserve the original cursor/mode header, followed by observable selection
/// metadata shared with the C Lua API. Raw anchor byte positions are not exposed
/// by C; range, cursor endpoint and anchored flag describe its orientation.
fn state(editor: &Editor) -> Result<String, ReplayError> {
    let data = editor.text.to_vec();
    let mut output = format!(
        "{}\n{}\nprimary {}\nselections {}\n",
        editor.cursor,
        editor.mode.name(),
        editor.primary_index(),
        editor.selections().len()
    );
    let visual = matches!(editor.mode, Mode::Visual | Mode::VisualLine);
    let line_ranges = if editor.mode == Mode::VisualLine {
        editor.selection_ranges()
    } else {
        Vec::new()
    };
    for (index, &selection) in editor.selections().iter().enumerate() {
        let range = if editor.mode == Mode::VisualLine {
            line_ranges[index].clone()
        } else {
            selection
                .range(&data)
                .map_err(|e| ReplayError::ordinary(e.to_string()))?
        };
        let endpoint = if !visual {
            "point"
        } else if selection.cursor == range.start {
            "start"
        } else {
            "end"
        };
        output.push_str(&format!(
            "selection {} {} {} {} {}\n",
            selection.cursor,
            range.start,
            range.end,
            usize::from(visual),
            endpoint
        ));
    }
    Ok(output)
}

fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let input = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| ReplayError::ordinary("usage: replay INPUT KEYS [--state PATH]"))?;
    let key_file = args
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| ReplayError::ordinary("usage: replay INPUT KEYS [--state PATH]"))?;
    let mut state = None;
    if let Some(flag) = args.next() {
        if flag != "--state" {
            return Err(ReplayError::ordinary("expected --state PATH").into());
        }
        state =
            Some(PathBuf::from(args.next().ok_or_else(|| {
                ReplayError::ordinary("missing state path")
            })?));
    }
    if args.next().is_some() {
        return Err(ReplayError::ordinary("unexpected argument").into());
    }
    let source = std::fs::read_to_string(key_file)?;
    let keys = keys(&source)?;
    let mut editor = Editor::new(Text::from_bytes(std::fs::read(input)?));
    let mut stopped = false;
    for (index, &key) in keys.iter().enumerate() {
        if stopped {
            return Err(
                ReplayError::ordinary(format!("keys remain after quit at key {index}")).into(),
            );
        }
        stopped = apply(&mut editor, key, index)?;
    }
    let before_escape = self::state(&editor)?;
    if !stopped {
        apply(&mut editor, Key::Escape, keys.len())?;
    }
    if let Some(state) = state {
        std::fs::write(
            state,
            format!("{}before_escape\n{}", self::state(&editor)?, before_escape),
        )?;
    }
    io::stdout().lock().write_all(&editor.text.to_vec())?;
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        let unsupported = error
            .downcast_ref::<ReplayError>()
            .is_some_and(|e| e.unsupported);
        eprintln!("replay: {error}");
        std::process::exit(if unsupported { 2 } else { 1 });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_formatting_and_literal_angles() {
        assert_eq!(
            keys("f<i<Space><Escape>  \n<C-r><Enter>\n").unwrap(),
            vec![
                Key::Char('f'),
                Key::Char('<'),
                Key::Char('i'),
                Key::Char(' '),
                Key::Escape,
                Key::Ctrl('r'),
                Key::Enter,
            ]
        );
        assert!(keys("<F999>").unwrap_err().unsupported);
    }

    #[test]
    fn modified_and_named_keys_keep_their_semantics() {
        assert_eq!(
            keys("<S-Tab><BackTab><M-C-j><A-C-K><M-A><A-é><PageUp><PageDown><Insert>").unwrap(),
            [
                Key::BackTab,
                Key::BackTab,
                Key::AltCtrl('j'),
                Key::AltCtrl('k'),
                Key::Alt('A'),
                Key::Alt('é'),
                Key::PageUp,
                Key::PageDown,
                Key::Insert
            ]
        );
        assert!(keys("<C-Up>").unwrap_err().unsupported);
        assert_eq!(
            keys("<F1><F20>").unwrap(),
            [Key::Function(1), Key::Function(20)]
        );
        assert_eq!(
            keys("<S-Left><S-Right><S-PageUp><S-PageDown>").unwrap(),
            [
                Key::ShiftLeft,
                Key::ShiftRight,
                Key::ShiftPageUp,
                Key::ShiftPageDown
            ]
        );
    }

    #[test]
    fn host_operations_are_explicitly_outside_replay() {
        let mut editor = Editor::new(Text::new());
        let result = keys(":new<Enter>")
            .unwrap()
            .into_iter()
            .enumerate()
            .try_for_each(|(index, key)| apply(&mut editor, key, index).map(|_| ()));
        assert!(result.unwrap_err().unsupported);
    }

    #[test]
    fn operational_errors_and_rejected_quit_keep_accepting_keys() {
        let mut editor = Editor::new(Text::new());
        let sequence = keys(":{<Enter>iok<Escape>:q<Enter>i!<Escape>").unwrap();
        for (index, key) in sequence.into_iter().enumerate() {
            assert!(!apply(&mut editor, key, index).unwrap());
        }
        assert_eq!(editor.text.to_vec(), b"ok!");
        assert!(editor.text.modified());
    }

    #[test]
    fn save_and_quit_requests_never_write_to_paths() {
        let mut editor = Editor::new(Text::new());
        let sequence = keys("ihello<Escape>:wq /never-open-this-path<Enter>").unwrap();
        let mut quit = false;
        for (index, key) in sequence.into_iter().enumerate() {
            quit = apply(&mut editor, key, index).unwrap();
        }
        assert!(quit);
        assert_eq!(editor.text.to_vec(), b"hello");
        assert!(!editor.text.modified());
    }

    #[test]
    fn state_records_all_selection_endpoints_before_escape() {
        let mut editor = Editor::new(Text::from_bytes(b"a b a"));
        for (index, key) in keys(":,x/a/<Enter>").unwrap().into_iter().enumerate() {
            apply(&mut editor, key, index).unwrap();
        }
        let before = state(&editor).unwrap();
        assert!(before.contains("primary 0\nselections 2\n"), "{before}");
        assert!(before.contains("selection 0 0 1 1 start\n"), "{before}");
        assert!(before.contains("selection 4 4 5 1 start\n"), "{before}");
        apply(&mut editor, Key::Escape, 100).unwrap();
        assert!(
            state(&editor)
                .unwrap()
                .contains("selection 4 4 5 0 point\n")
        );
    }

    #[test]
    fn state_distinguishes_oriented_visual_selections() {
        let mut editor = Editor::new(Text::from_bytes(b"one two\n"));
        for (index, key) in keys("v2lo").unwrap().into_iter().enumerate() {
            apply(&mut editor, key, index).unwrap();
        }
        assert!(
            state(&editor)
                .unwrap()
                .contains("selection 0 0 3 1 start\n")
        );
        apply(&mut editor, Key::Char('o'), 100).unwrap();
        assert!(state(&editor).unwrap().contains("selection 2 0 3 1 end\n"));
    }
}

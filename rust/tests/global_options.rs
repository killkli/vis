//! Native global options that change how the editor runs.

use vis::editor::Key;
use vis::workspace::Workspace;

fn workspace() -> Workspace {
    Workspace::new(&[], Some(b"one\ntwo\n".to_vec()), false).unwrap()
}

fn command(ws: &mut Workspace, text: &str) {
    for character in text.chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
    ws.handle_key(Key::Enter).unwrap();
}

#[test]
fn the_shell_option_selects_the_program_for_external_commands() {
    let mut ws = workspace();
    let default = ws.editor.shell();
    assert!(!default.is_empty(), "a shell program is always resolved");
    command(&mut ws, ":set shell /bin/dash");
    assert_eq!(ws.editor.shell(), "/bin/dash");
}

#[test]
fn the_escape_delay_option_is_reported_in_milliseconds() {
    let mut ws = workspace();
    assert!(ws.editor.escape_delay().is_none());
    command(&mut ws, ":set escdelay 50");
    assert_eq!(
        ws.editor.escape_delay(),
        Some(std::time::Duration::from_millis(50))
    );
}

#[test]
fn the_ignorecase_option_makes_the_search_case_insensitive() {
    let mut ws = Workspace::new(&[], Some(b"Hello World\n".to_vec()), false).unwrap();
    // Without the option the search is case sensitive and finds nothing.
    assert!(ws.handle_key(Key::Char('/')).is_ok());
    for character in "world".chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
    ws.handle_key(Key::Enter).unwrap();
    assert_eq!(
        ws.editor.cursor, 0,
        "the case sensitive search finds nothing"
    );
    ws.command("set ignorecase").unwrap();
    assert!(ws.editor.ignorecase(), "ignorecase option");
    assert!(ws.handle_key(Key::Char('/')).is_ok());
    for character in "world".chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
    ws.handle_key(Key::Enter).unwrap();
    assert_eq!(ws.editor.cursor, 6, "the match starts at the second word");
}

#[test]
fn an_invalid_escape_delay_is_rejected() {
    let mut ws = workspace();
    command(&mut ws, ":set escdelay soon");
    assert!(ws.editor.escape_delay().is_none());
}

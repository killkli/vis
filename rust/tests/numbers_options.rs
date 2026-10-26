//! The native line number options reach the window display.

use vis::editor::Key;
use vis::workspace::Workspace;

fn workspace() -> Workspace {
    Workspace::new(&[], Some(b"one\ntwo\n".to_vec()), false).unwrap()
}

fn prompt(ws: &mut Workspace, command: &str) {
    for character in command.chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
    ws.handle_key(Key::Enter).unwrap();
}

#[test]
fn the_number_options_reach_the_window_display() {
    let mut ws = workspace();
    prompt(&mut ws, ":set numbers");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(display.numbers, "absolute numbers");
    assert!(!display.relativenumbers, "relative numbers are exclusive");
    prompt(&mut ws, ":set relativenumbers");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(display.relativenumbers, "relative numbers");
    assert!(!display.numbers, "absolute numbers are cleared");
    prompt(&mut ws, ":set numberwidth 6");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.numberwidth, 6);
}

#[test]
fn the_window_display_options_are_reachable_from_the_prompt() {
    let mut ws = workspace();
    prompt(&mut ws, ":set statusbar off");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(!display.statusbar, "status bar");
    prompt(&mut ws, ":set showeof off");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(!display.showeof, "end of file marker");
}

//! The native window options reach the window display.

use vis::editor::Key;
use vis::workspace::Workspace;

fn workspace() -> Workspace {
    Workspace::new(&[], Some(b"one\ntwo\nthree\n".to_vec()), false).unwrap()
}

fn prompt(ws: &mut Workspace, command: &str) {
    for character in command.chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
    ws.handle_key(Key::Enter).unwrap();
}

#[test]
fn the_remaining_native_window_options_are_accepted() {
    let mut ws = workspace();
    prompt(&mut ws, ":set cursorline on");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(display.cursorline, "cursor line");
    prompt(&mut ws, ":set colorcolumn 12");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.colorcolumn, 12, "color column");
    prompt(&mut ws, ":set showspaces on");
    prompt(&mut ws, ":set showtabs on");
    prompt(&mut ws, ":set shownewlines on");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert!(display.showspaces, "show spaces");
    assert!(display.showtabs, "show tabs");
    assert!(display.shownewlines, "show newlines");
    prompt(&mut ws, ":set wrapcolumn 20");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.wrapcolumn, 20, "wrap column");
}

#[test]
fn the_break_characters_option_is_accepted() {
    let mut ws = workspace();
    prompt(&mut ws, ":set breakat ,");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.breakat.as_ref(), ",");
    assert_eq!(ws.editor.breakat(), ",");
}

#[test]
fn an_invalid_option_value_is_rejected() {
    let mut ws = workspace();
    prompt(&mut ws, ":set colorcolumn soon");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.colorcolumn, 0, "color column");
    prompt(&mut ws, ":set wrapcolumn soon");
    let display = ws.window_display(ws.active_window_id()).unwrap();
    assert_eq!(display.wrapcolumn, 0, "wrap column");
}

//! Visual-mode selection flips through the real key host.

use vis::editor::Key;
use vis::workspace::Workspace;

fn workspace(text: &[u8]) -> Workspace {
    Workspace::new(&[], Some(text.to_vec()), false).unwrap()
}

fn keys(ws: &mut Workspace, text: &str) {
    for character in text.chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
}

#[test]
fn visual_flip_moves_every_cursor_to_the_other_end() {
    let mut ws = workspace(b"abc\ndef\nghi\njkl\n");
    ws.handle_key(Key::Ctrl('j')).unwrap();
    ws.handle_key(Key::Ctrl('j')).unwrap();
    let starts: Vec<usize> = ws.editor.selections().iter().map(|s| s.cursor).collect();
    assert_eq!(starts, [0, 4, 8]);

    keys(&mut ws, "vl");
    let extended: Vec<usize> = ws.editor.selections().iter().map(|s| s.cursor).collect();
    assert_eq!(extended, [1, 5, 9]);
    keys(&mut ws, "o");
    let flipped: Vec<usize> = ws.editor.selections().iter().map(|s| s.cursor).collect();
    assert_eq!(flipped, [0, 4, 8]);
    assert_eq!(ws.editor.selection_ranges(), vec![0..2, 4..6, 8..10]);
}

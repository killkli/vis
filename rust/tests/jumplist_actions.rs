//! Native jump-list behavior through the real key, command and window host.

use vis::editor::{Key, Mode};
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
fn save_and_navigate_follow_native_ring_order() {
    let mut ws = workspace(b"abcdefghijklmnopqrstuvwxyz");
    keys(&mut ws, "gs");
    keys(&mut ws, "lllllgs");
    keys(&mut ws, "llllllllllgs");
    assert_eq!(ws.editor.cursor, 15);

    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 15, "equal slot geometry does not move");
    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 5, "older save follows");
    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 0, "oldest save follows");
    keys(&mut ws, "g>");
    assert_eq!(ws.editor.cursor, 0, "equal geometry does not move");
    keys(&mut ws, "g>");
    assert_eq!(ws.editor.cursor, 5, "next newer save is restored");
    keys(&mut ws, "g>");
    assert_eq!(ws.editor.cursor, 15, "newest save is restored");
}

#[test]
fn leaving_visual_mode_stores_geometry_and_mode() {
    let mut ws = workspace(b"abcdefghij\n");
    keys(&mut ws, "vll");
    ws.handle_key(Key::Escape).unwrap();
    assert_eq!(ws.editor.cursor, 2);
    keys(&mut ws, "lll");
    assert_eq!(ws.editor.cursor, 5);

    keys(&mut ws, "g<");
    assert_eq!(ws.editor.mode, Mode::Visual);
    assert_eq!(ws.editor.selection_ranges(), vec![0..3]);
    assert_eq!(ws.editor.cursor, 2);
}

#[test]
fn jump_motions_record_their_destination() {
    let mut ws = workspace(b"a\nb\nc\nd\ne\n");
    keys(&mut ws, "G");
    assert_eq!(ws.editor.cursor, 8);
    keys(&mut ws, "k");
    assert_eq!(ws.editor.cursor, 6);
    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 8);
}

#[test]
fn split_command_seeds_the_new_window_and_keeps_lists_separate() {
    let mut ws = workspace(b"abcdefghijklmnopqrstuvwxyz0123456789");
    let original = ws.active_window_id();
    keys(&mut ws, "lllllgs");
    keys(&mut ws, "lllllgs");
    assert_eq!(ws.editor.cursor, 10);

    ws.command("split").unwrap();
    let split = ws.active_window_id();
    assert_ne!(split, original);
    assert_eq!(ws.editor.cursor, 10, "split copies the source cursor");

    keys(&mut ws, "llllllllll");
    assert_eq!(ws.editor.cursor, 20);
    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 10, "command completion seeds the split");
    keys(&mut ws, "g<");
    assert_eq!(
        ws.editor.cursor, 10,
        "the split does not inherit the source history"
    );

    ws.focus_window(original).unwrap();
    keys(&mut ws, "llllllllll");
    assert_eq!(ws.editor.cursor, 20);
    keys(&mut ws, "g<");
    assert_eq!(
        ws.editor.cursor, 10,
        "original window keeps its own history"
    );
    keys(&mut ws, "g<");
    assert_eq!(ws.editor.cursor, 5);
}

//! The first visible row is anchored to its content, like native
//! `view->start_mark`.

use vis::editor::Key;
use vis::view::ViewportMetrics;
use vis::workspace::Workspace;

fn workspace(text: &[u8]) -> Workspace {
    Workspace::new(&[], Some(text.to_vec()), false).unwrap()
}

fn numbered_lines(count: usize) -> String {
    let mut text = String::new();
    for line in 0..count {
        text.push_str(&format!("line {line:03}\n"));
    }
    text
}

fn measure(ws: &mut Workspace, top: usize) {
    let id = ws.active_window_id();
    ws.set_window_viewport(
        id,
        ViewportMetrics {
            width: 80,
            height: 23,
            top,
        },
    );
}

#[test]
fn the_anchored_line_follows_edits_above_the_view() {
    let text = numbered_lines(100);
    let mut ws = workspace(text.as_bytes());
    measure(&mut ws, 38);
    assert_eq!(ws.editor.view_anchor_row(), Some(38));
    // A line inserted above the view moves the anchored content down, the
    // way a native text mark does.
    ws.editor.text.insert(0, b"new\n").unwrap();
    assert_eq!(ws.editor.view_anchor_row(), Some(39));
    // Removing it again restores the original row.
    ws.editor.text.delete(0..4).unwrap();
    assert_eq!(ws.editor.view_anchor_row(), Some(38));
}

#[test]
fn the_anchor_is_dropped_when_its_line_disappears() {
    let text = numbered_lines(100);
    let mut ws = workspace(text.as_bytes());
    measure(&mut ws, 38);
    assert_eq!(ws.editor.view_anchor_row(), Some(38));
    // Deleting the anchored line leaves no mark to follow.
    ws.editor.text.delete(0..9).unwrap();
    assert!(ws.editor.view_anchor_row().is_none_or(|row| row <= 38));
}

#[test]
fn the_anchor_survives_a_window_switch() {
    let text = numbered_lines(100);
    let mut ws = workspace(text.as_bytes());
    measure(&mut ws, 38);
    assert_eq!(ws.editor.view_anchor_row(), Some(38));
    ws.editor.text.insert(0, b"new\n").unwrap();
    ws.handle_key(Key::Char('w')).unwrap();
    ws.handle_key(Key::Char('w')).unwrap();
    assert_eq!(ws.editor.view_anchor_row(), Some(39));
}

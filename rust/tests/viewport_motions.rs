//! Screen-line and window-line motions using measured body geometry.

use vis::editor::{Key, NativeAction};
use vis::view::ViewportMetrics;
use vis::workspace::Workspace;

fn workspace(text: &[u8]) -> Workspace {
    Workspace::new(&[], Some(text.to_vec()), false).unwrap()
}

fn measure(ws: &mut Workspace, width: usize, height: usize, top: usize) {
    let id = ws.active_window_id();
    ws.set_window_viewport(id, ViewportMetrics { width, height, top });
}

fn action(ws: &mut Workspace, action: NativeAction) {
    ws.handle_key(Key::Native(action)).unwrap();
}

fn keys(ws: &mut Workspace, text: &str) {
    for character in text.chars() {
        ws.handle_key(Key::Char(character)).unwrap();
    }
}

#[test]
fn window_lines_land_on_measured_rows() {
    let mut ws = workspace(b"l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n");
    measure(&mut ws, 10, 5, 1);
    action(&mut ws, NativeAction::CursorWindowLineTop);
    assert_eq!(ws.editor.cursor, 3, "first visible row");
    action(&mut ws, NativeAction::CursorWindowLineMiddle);
    assert_eq!(ws.editor.cursor, 6, "middle visible row");
    action(&mut ws, NativeAction::CursorWindowLineBottom);
    assert_eq!(ws.editor.cursor, 15, "last visible row");
}

#[test]
fn window_line_counts_select_relative_rows() {
    let mut ws = workspace(b"l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n");
    measure(&mut ws, 10, 5, 1);
    ws.handle_key(Key::Char('2')).unwrap();
    action(&mut ws, NativeAction::CursorWindowLineTop);
    assert_eq!(ws.editor.cursor, 6, "count selects the second visible row");
    ws.handle_key(Key::Char('2')).unwrap();
    action(&mut ws, NativeAction::CursorWindowLineBottom);
    assert_eq!(
        ws.editor.cursor, 9,
        "count moves the window bottom line upwards"
    );
}

#[test]
fn window_lines_clamp_at_the_end_of_the_text() {
    let mut ws = workspace(b"one\ntwo\n");
    measure(&mut ws, 10, 8, 0);
    action(&mut ws, NativeAction::CursorWindowLineBottom);
    assert_eq!(
        ws.editor.cursor, 4,
        "clamped to the last source row, not the EOF cursor row"
    );
}

#[test]
fn undrawn_windows_use_native_no_terminal_geometry() {
    // Native vis falls back to 80 columns and 23 body rows when no terminal
    // size is available, so the same defaults keep headless actions coherent.
    let mut ws = workspace(b"one\ntwo\nthree\n");
    action(&mut ws, NativeAction::CursorWindowLineBottom);
    assert_eq!(ws.editor.cursor, 8, "L clamps to the last source row");
    keys(&mut ws, "gg");
    action(&mut ws, NativeAction::CursorScreenLineEnd);
    assert_eq!(
        ws.editor.cursor, 3,
        "the default 80-column row ends on the newline cell"
    );
}

#[test]
fn screen_lines_use_wrapped_columns_and_rows() {
    let mut ws = workspace(b"abcdefghij");
    measure(&mut ws, 4, 3, 0);
    action(&mut ws, NativeAction::CursorScreenLineEnd);
    assert_eq!(ws.editor.cursor, 3, "end of the first wrapped row");
    action(&mut ws, NativeAction::CursorScreenLineMiddle);
    assert_eq!(ws.editor.cursor, 2, "middle cell of the first row");
    action(&mut ws, NativeAction::CursorScreenLineDown);
    assert_eq!(ws.editor.cursor, 6, "same display column on the next row");
    action(&mut ws, NativeAction::CursorScreenLineBegin);
    assert_eq!(ws.editor.cursor, 4, "row start");
    keys(&mut ws, "2");
    action(&mut ws, NativeAction::CursorScreenLineDown);
    assert_eq!(ws.editor.cursor, 9, "counted rows keep the display column");
    action(&mut ws, NativeAction::CursorScreenLineUp);
    assert_eq!(ws.editor.cursor, 6, "one row upwards");
    action(&mut ws, NativeAction::CursorScreenLineBegin);
    assert_eq!(ws.editor.cursor, 4);
    action(&mut ws, NativeAction::CursorScreenLineUp);
    assert_eq!(
        ws.editor.cursor, 2,
        "the preferred column survives row starts"
    );
}

#[test]
fn screen_line_column_preference_survives_wide_tokens() {
    let mut ws = workspace("ab界c".as_bytes());
    measure(&mut ws, 3, 3, 0);
    ws.handle_key(Key::Char('l')).unwrap();
    assert_eq!(ws.editor.cursor, 1);
    action(&mut ws, NativeAction::CursorScreenLineDown);
    assert_eq!(
        ws.editor.cursor, 2,
        "column one lands on the wide base cell"
    );
    action(&mut ws, NativeAction::CursorScreenLineUp);
    assert_eq!(ws.editor.cursor, 1);
    action(&mut ws, NativeAction::CursorScreenLineEnd);
    assert_eq!(ws.editor.cursor, 1, "end of the two-column row");
}

#[test]
fn native_h_and_l_keys_use_measured_geometry() {
    let mut ws = workspace(b"l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n");
    measure(&mut ws, 10, 5, 1);
    keys(&mut ws, "H");
    assert_eq!(ws.editor.cursor, 3, "H selects the first visible row");
    keys(&mut ws, "L");
    assert_eq!(ws.editor.cursor, 15, "L selects the last visible row");
}

#[test]
fn g_screen_line_keys_move_wrapped_rows() {
    let mut ws = workspace(b"abcdefghij");
    measure(&mut ws, 4, 3, 0);
    keys(&mut ws, "gj");
    assert_eq!(ws.editor.cursor, 4, "gj moves one display row down");
    keys(&mut ws, "gk");
    assert_eq!(ws.editor.cursor, 0, "gk moves back up");
    keys(&mut ws, "g$");
    assert_eq!(ws.editor.cursor, 3, "g$ ends the display row");
    keys(&mut ws, "gm");
    assert_eq!(ws.editor.cursor, 2, "gm centers within the display row");
}

#[test]
fn redraw_actions_request_rows_and_frames_consume_them() {
    let mut lines = String::new();
    for line in 0..100 {
        lines.push_str(&format!("l{line}\n"));
    }
    let mut ws = workspace(lines.as_bytes());
    measure(&mut ws, 10, 5, 0);
    keys(&mut ws, "G");
    action(&mut ws, NativeAction::WindowRedrawTop);
    assert_eq!(
        ws.editor.scroll_request(),
        Some(99),
        "cursor row becomes top"
    );
    assert_eq!(ws.frames()[0].topline, Some(99));
    ws.clear_scroll_requests();
    assert_eq!(ws.editor.scroll_request(), None, "requests are one-shot");
    action(&mut ws, NativeAction::WindowRedrawCenter);
    assert_eq!(ws.editor.scroll_request(), Some(97));
    action(&mut ws, NativeAction::WindowRedrawBottom);
    assert_eq!(ws.editor.scroll_request(), Some(95));
    action(&mut ws, NativeAction::Redraw);
    assert_eq!(
        ws.editor.scroll_request(),
        Some(95),
        "plain redraw keeps scroll"
    );
}

#[test]
fn redraw_keys_request_cursor_relative_rows() {
    let mut lines = String::new();
    for line in 0..100 {
        lines.push_str(&format!("l{line}\n"));
    }
    let mut ws = workspace(lines.as_bytes());
    measure(&mut ws, 10, 5, 0);
    keys(&mut ws, "G");
    keys(&mut ws, "zt");
    assert_eq!(ws.editor.scroll_request(), Some(99));
    keys(&mut ws, "zz");
    assert_eq!(ws.editor.scroll_request(), Some(97));
    keys(&mut ws, "zb");
    assert_eq!(ws.editor.scroll_request(), Some(95));
}

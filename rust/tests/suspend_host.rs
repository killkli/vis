//! Native `vis-suspend` routing through the host plan.

use vis::editor::{Key, Mode};
use vis::workspace::{Outcome, Workspace};

fn workspace() -> Workspace {
    Workspace::new(&[], Some(b"abc\ndef\n".to_vec()), false).unwrap()
}

#[test]
fn ctrl_z_returns_the_suspend_outcome_without_editing() {
    let mut ws = workspace();
    assert_eq!(ws.handle_key(Key::Ctrl('z')).unwrap(), Outcome::Suspend);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.cursor, 0);
    assert_eq!(ws.editor.text.to_vec(), b"abc\ndef\n");
    // Editing continues normally after a suspend.
    ws.handle_key(Key::Char('l')).unwrap();
    assert_eq!(ws.editor.cursor, 1);
}

#[test]
fn suspend_does_not_fire_outside_normal_mode() {
    let mut ws = workspace();
    ws.handle_key(Key::Char('i')).unwrap();
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert!(!matches!(
        ws.handle_key(Key::Ctrl('z')),
        Ok(Outcome::Suspend)
    ));
}

#[test]
fn a_mapping_can_shadow_the_suspend_binding() {
    let mut ws = workspace();
    ws.command("map normal <C-z> l").unwrap();
    assert_eq!(ws.handle_key(Key::Ctrl('z')).unwrap(), Outcome::Continue);
    assert_eq!(ws.editor.cursor, 1);
}

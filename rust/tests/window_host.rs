use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use vis::editor::Mode;
use vis::selection::Selection;
use vis::workspace::{HostPlan, HostStep, Outcome, WindowDisplay, Workspace};

fn workspace() -> Workspace {
    Workspace::new(&[], Some(b"abcdefgh\n".to_vec()), false).unwrap()
}

fn finish(ws: &mut Workspace, plan: &mut HostPlan) {
    loop {
        match ws.advance(plan, true).unwrap() {
            HostStep::Complete(_) => break,
            HostStep::FileOpen(_)
            | HostStep::WindowOpen(_)
            | HostStep::WindowClose(_)
            | HostStep::FileClose(_) => {}
            _ => panic!("unexpected host operation"),
        }
    }
}

#[test]
fn windows_prepend_and_focus_and_close_preserve_global_insert_and_counts() {
    let mut ws = workspace();
    let first = ws.active_window_id();
    ws.open_scratch(None, false).unwrap();
    let second = ws.active_window_id();
    ws.open_scratch(None, false).unwrap();
    let third = ws.active_window_id();
    assert_eq!(ws.window_ids(), [third, second, first]);
    ws.editor.switch_mode(Mode::Insert).unwrap();
    ws.editor.set_count(Some(3)).unwrap();
    ws.focus_window(first).unwrap();
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert_eq!(ws.editor.current_count(), Some(3));
    ws.command("wincmd w").unwrap();
    assert_eq!(ws.active_window_id(), third);
    assert_eq!(ws.editor.mode, Mode::Insert);
    ws.command("wincmd p").unwrap();
    assert_eq!(ws.active_window_id(), first);
    ws.focus_window(second).unwrap();
    assert_eq!(ws.command("q!").unwrap(), Outcome::Continue);
    assert_eq!(
        ws.active_window_id(),
        first,
        "close chooses native next window"
    );
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert_eq!(ws.editor.current_count(), Some(3));
    assert_eq!(ws.window_ids(), [third, first]);
}

#[test]
fn frame_anchors_belong_to_each_window_while_mode_is_global() {
    let mut ws = workspace();
    let first = ws.active_window_id();
    ws.editor.switch_mode(Mode::Visual).unwrap();
    ws.open_scratch(None, false).unwrap();
    let second = ws.active_window_id();
    let frames = ws.frames();
    assert!(frames.iter().all(|frame| frame.mode == Mode::Visual));
    assert_eq!(
        frames
            .iter()
            .find(|frame| frame.id == first)
            .unwrap()
            .anchored,
        [true]
    );
    assert_eq!(
        frames
            .iter()
            .find(|frame| frame.id == second)
            .unwrap()
            .anchored,
        [false]
    );
    assert!(
        !frames
            .iter()
            .find(|frame| frame.id == first)
            .unwrap()
            .selections_visible
    );
    assert!(
        frames
            .iter()
            .find(|frame| frame.id == second)
            .unwrap()
            .selections_visible
    );
    ws.editor.switch_mode(Mode::Normal).unwrap();
    ws.focus_window(first).unwrap();
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(
        ws.frames()
            .iter()
            .find(|frame| frame.id == first)
            .unwrap()
            .anchored,
        [true]
    );
}

#[test]
fn split_copies_primary_ui_and_local_maps_after_open_callback_and_retains_chosen_focus() {
    let mut ws = workspace();
    let source = ws.active_window_id();
    let source_primary = ws.window_primary_selection(source).unwrap();
    ws.editor
        .update_selection(
            source_primary,
            Selection {
                anchor: 1,
                cursor: 1,
            },
        )
        .unwrap();
    ws.editor
        .create_selection(
            Selection {
                anchor: 6,
                cursor: 6,
            },
            false,
        )
        .unwrap();
    ws.set_window_options(source, 4, true, true).unwrap();
    let flags = WindowDisplay {
        oneline: false,
        statusbar: false,
        showeof: false,
    };
    ws.set_window_display(source, flags).unwrap();
    ws.editor
        .map_alias(Mode::Normal, "Q", "iS<Escape>", Some(source as u64))
        .unwrap();
    let mut split = ws.begin_command("split").unwrap();
    let target = match ws.advance(&mut split, true).unwrap() {
        HostStep::WindowOpen(window) => window,
        _ => panic!("expected split window open"),
    };
    assert_eq!(ws.editor.cursor, 0);
    assert_eq!(ws.editor.selections().len(), 1);
    assert_eq!(ws.window_options(target), Some((8, false, true)));
    assert_eq!(
        ws.window_display(target),
        Some(WindowDisplay {
            showeof: false,
            ..WindowDisplay::default()
        })
    );
    let added = ws
        .editor
        .create_selection(
            Selection {
                anchor: 5,
                cursor: 5,
            },
            false,
        )
        .unwrap();
    ws.editor
        .map_alias(Mode::Normal, "Q", "iT<Escape>", Some(target as u64))
        .unwrap();
    ws.window_update_selection(
        source,
        source_primary,
        Selection {
            anchor: 3,
            cursor: 3,
        },
    )
    .unwrap();
    ws.open_scratch(None, false).unwrap();
    let chosen = ws.active_window_id();
    finish(&mut ws, &mut split);
    assert_eq!(ws.active_window_id(), chosen);
    assert_eq!(ws.window_options(target), Some((8, false, true)));
    assert_eq!(ws.window_display(target), Some(flags));
    let target_primary = ws.window_primary_selection(target).unwrap();
    assert_eq!(
        ws.window_selection_position(target, target_primary),
        Some(3)
    );
    assert_eq!(ws.window_selection_position(target, added), Some(5));
    assert_eq!(ws.window_identified_selections(target).unwrap().len(), 2);
    assert_eq!(
        ws.editor.mapping_aliases(Mode::Normal, Some(target as u64)),
        ws.editor.mapping_aliases(Mode::Normal, Some(source as u64))
    );
}

#[test]
fn prompt_moves_to_native_tail_and_expanded_history_keeps_its_list_position() {
    let mut ws = workspace();
    let first = ws.active_window_id();
    ws.open_scratch(None, false).unwrap();
    let second = ws.active_window_id();
    ws.handle_key(vis::editor::Key::Char(':')).unwrap();
    let prompt = ws.active_window_id();
    assert_eq!(ws.window_ids(), [second, first, prompt]);
    let frames = ws.frames();
    assert!(
        frames
            .iter()
            .find(|frame| frame.id == prompt)
            .unwrap()
            .selections_visible
    );
    assert!(
        frames
            .iter()
            .find(|frame| frame.id == second)
            .unwrap()
            .selections_visible
    );
    assert!(
        !frames
            .iter()
            .find(|frame| frame.id == first)
            .unwrap()
            .selections_visible
    );
    ws.handle_key(vis::editor::Key::Up).unwrap();
    assert_eq!(ws.window_ids(), [second, first, prompt]);
    assert!(!ws.window_display(prompt).unwrap().oneline);
    ws.handle_key(vis::editor::Key::Escape).unwrap();
    ws.handle_key(vis::editor::Key::Escape).unwrap();
    assert_eq!(ws.active_window_id(), second);
    assert_eq!(ws.window_ids(), [second, first]);
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vis-window-host-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn edit_new_file_swaps_into_old_slot_and_close_callback_sees_old_focus() {
    let directory = Directory::new();
    let path = directory.0.join("replacement");
    std::fs::write(&path, b"replacement").unwrap();
    let mut ws = workspace();
    let first = ws.active_window_id();
    ws.open_scratch(None, false).unwrap();
    let old = ws.active_window_id();
    ws.open_scratch(None, false).unwrap();
    let third = ws.active_window_id();
    ws.focus_window(old).unwrap();
    let mut edit = ws
        .begin_command(&format!("edit {}", path.display()))
        .unwrap();
    assert!(matches!(
        ws.advance(&mut edit, true).unwrap(),
        HostStep::FileOpen(_)
    ));
    let new = match ws.advance(&mut edit, true).unwrap() {
        HostStep::WindowOpen(window) => window,
        _ => panic!("expected replacement open"),
    };
    assert_eq!(ws.window_ids(), [new, third, old, first]);
    assert!(
        matches!(ws.advance(&mut edit, true).unwrap(), HostStep::WindowClose(window) if window == old)
    );
    assert_eq!(ws.active_window_id(), old);
    assert_eq!(ws.window_ids(), [old, third, new, first]);
    finish(&mut ws, &mut edit);
    assert_eq!(ws.active_window_id(), new);
    assert_eq!(ws.window_ids(), [third, new, first]);
    assert_eq!(ws.editor.text.to_vec(), b"replacement");
}

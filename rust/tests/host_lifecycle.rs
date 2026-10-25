//! Core lifecycle continuation tests: callbacks mutate the host between steps,
//! with no Lua dependency and no borrow held across the callback boundary.
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use vis::editor::{CallbackResponse, CallbackType, EditorAction, Key, Mode};
use vis::workspace::{HostPlan, HostStep, Outcome, Workspace};

#[derive(Debug, PartialEq, Eq)]
enum Event {
    FileOpen(usize),
    WindowOpen(usize),
    WindowClose(usize),
    FileClose(usize),
    SavePre(usize),
    SavePost(usize),
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vis-host-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(std::fs::canonicalize(path).unwrap())
    }
    fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn drive(workspace: &mut Workspace, plan: &mut HostPlan) -> io::Result<(Vec<Event>, Outcome)> {
    let mut events = Vec::new();
    loop {
        events.push(match workspace.advance(plan, true)? {
            HostStep::FileOpen(id) => Event::FileOpen(id),
            HostStep::WindowOpen(id) => Event::WindowOpen(id),
            HostStep::WindowClose(id) => Event::WindowClose(id),
            HostStep::FileClose(id) => Event::FileClose(id),
            HostStep::SavePre(save) => Event::SavePre(save.buffer),
            HostStep::SavePost { buffer, .. } => Event::SavePost(buffer),
            HostStep::Complete(outcome) => return Ok((events, outcome)),
            HostStep::PromptCommand { token, command } => {
                let success = match workspace.prepare_command(&command) {
                    Ok(mut nested) => match drive(workspace, &mut nested) {
                        Ok((nested_events, _)) => {
                            events.extend(nested_events);
                            true
                        }
                        Err(_) => {
                            workspace.cancel_plan(&nested);
                            true
                        }
                    },
                    Err(_) => false,
                };
                workspace.resume_prompt_command(plan, token, success)?;
                continue;
            }
            HostStep::TargetCommand { token, request } => {
                let success = match workspace.execute_target_command(&request) {
                    Ok(EditorAction::Continue) => true,
                    Ok(EditorAction::Message(_)) => false,
                    Ok(_) => panic!("unexpected target action in native lifecycle test"),
                    Err(error) => {
                        workspace.cancel_plan(plan);
                        return Err(error);
                    }
                };
                workspace.resume_target_command(plan, token, success)?;
                continue;
            }
            HostStep::Input(_) => panic!("unexpected intercepted input in native host test"),
            HostStep::Callback(_) => panic!("unexpected Lua callback in a native host test"),
            HostStep::Command(command) => {
                panic!("unexpected custom command in a native host test: {command}")
            }
        });
    }
}

fn open(workspace: &mut Workspace, path: &Path) -> (usize, usize, Vec<Event>) {
    let mut plan = workspace.begin_open(path, None, false).unwrap();
    let (events, _) = drive(workspace, &mut plan).unwrap();
    let window = workspace.active_window_id();
    (workspace.window_file_id(window).unwrap(), window, events)
}

#[test]
fn init_is_empty_then_file_open_precedes_window_open() {
    let dir = Directory::new();
    let path = dir.file("input", b"bytes");
    let mut workspace = Workspace::empty().unwrap();
    assert_eq!(workspace.window_count(), 0);
    assert!(workspace.window_ids().is_empty());
    assert!(workspace.file_ids().is_empty());
    assert!(workspace.frames().is_empty());
    assert!(!workspace.modified());
    assert_eq!(workspace.path(), None);
    let mut plan = workspace.begin_open(&path, None, false).unwrap();
    let file = match workspace.advance(&mut plan, true).unwrap() {
        HostStep::FileOpen(file) => file,
        _ => panic!("FileOpen must precede WindowOpen"),
    };
    assert_eq!(workspace.window_count(), 0);
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"bytes".to_vec())
    );
    let window = match workspace.advance(&mut plan, true).unwrap() {
        HostStep::WindowOpen(window) => window,
        _ => panic!("expected WindowOpen after FileOpen"),
    };
    assert_eq!(workspace.window_file_id(window), Some(file));
    assert!(matches!(
        workspace.advance(&mut plan, true).unwrap(),
        HostStep::Complete(Outcome::Continue)
    ));
}

#[test]
fn shared_opens_emit_file_open_once_and_split_has_distinct_selection_ids() {
    let dir = Directory::new();
    let path = dir.file("input", b"one\ntwo\n");
    let mut workspace = Workspace::empty().unwrap();
    let (file, first, events) = open(&mut workspace, &path);
    assert_eq!(
        events,
        vec![Event::FileOpen(file), Event::WindowOpen(first)]
    );
    let (same, second, events) = open(&mut workspace, &path);
    assert_eq!(same, file);
    assert_eq!(events, vec![Event::WindowOpen(second)]);
    let mut split = workspace.begin_command("split").unwrap();
    let (events, _) = drive(&mut workspace, &mut split).unwrap();
    let third = workspace.active_window_id();
    assert_eq!(events, vec![Event::WindowOpen(third)]);
    assert_eq!(workspace.window_file_id(third), Some(file));
    assert_eq!(workspace.file_ids(), vec![file]);
    assert_ne!(
        workspace.window_primary_selection(second),
        workspace.window_primary_selection(third)
    );
}

#[test]
fn keyboard_host_command_yields_the_same_owned_open_events() {
    let mut workspace = Workspace::empty().unwrap();
    let mut initial = workspace.begin_scratch(None, false).unwrap();
    drive(&mut workspace, &mut initial).unwrap();
    for key in ":new".chars().map(Key::Char) {
        let mut plan = workspace.begin_key(key).unwrap();
        assert!(drive(&mut workspace, &mut plan).unwrap().0.is_empty());
    }
    let mut plan = workspace.begin_key(Key::Enter).unwrap();
    let (events, _) = drive(&mut workspace, &mut plan).unwrap();
    assert!(matches!(
        events.as_slice(),
        [Event::FileOpen(_), Event::WindowOpen(_)]
    ));
    assert_eq!(workspace.window_count(), 2);
}

#[test]
fn owned_mapping_callback_can_mutate_its_buffer_before_resume() {
    let mut workspace = Workspace::empty().unwrap();
    let mut initial = workspace
        .begin_scratch(Some(b"base".to_vec()), false)
        .unwrap();
    drive(&mut workspace, &mut initial).unwrap();
    let window = workspace.active_window_id();
    let file = workspace.window_file_id(window).unwrap();
    workspace
        .editor
        .register_callback(CallbackType::Mapping, 17)
        .unwrap();
    workspace
        .editor
        .map_callback(Mode::Normal, "Q", 17, None)
        .unwrap();
    let mut plan = workspace.begin_key(Key::Char('Q')).unwrap();
    let request = match workspace.advance(&mut plan, true).unwrap() {
        HostStep::Callback(request) => request,
        _ => panic!("expected an owned callback request"),
    };
    assert_eq!(request.callback, 17);
    workspace
        .with_buffer_mut(file, |text| text.insert(0, b"callback ").unwrap())
        .unwrap();
    workspace
        .resume_callback(&mut plan, request.token, CallbackResponse::Mapping(Some(0)))
        .unwrap();
    drive(&mut workspace, &mut plan).unwrap();
    assert_eq!(workspace.window_file_id(window), Some(file));
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"callback base".to_vec())
    );
}

#[test]
fn close_events_keep_window_file_and_selection_handles_live_until_commit() {
    let dir = Directory::new();
    let path = dir.file("input", b"content");
    let mut workspace = Workspace::empty().unwrap();
    let (file, window, _) = open(&mut workspace, &path);
    let selection = workspace.window_primary_selection(window).unwrap();
    let mut close = workspace.begin_close_window(window, false).unwrap();
    assert!(
        matches!(workspace.advance(&mut close, true).unwrap(), HostStep::WindowClose(id) if id == window)
    );
    assert_eq!(workspace.window_file_id(window), Some(file));
    assert_eq!(
        workspace.window_selection_position(window, selection),
        Some(0)
    );
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"content".to_vec())
    );
    assert!(
        matches!(workspace.advance(&mut close, true).unwrap(), HostStep::FileClose(id) if id == file)
    );
    assert_eq!(workspace.window_file_id(window), Some(file));
    assert!(workspace.with_buffer(file, |text| text.len()).is_some());
    assert!(matches!(
        workspace.advance(&mut close, true).unwrap(),
        HostStep::Complete(Outcome::Quit)
    ));
    assert_eq!(workspace.window_file_id(window), None);
    assert_eq!(workspace.window_selection_position(window, selection), None);
    assert!(workspace.with_buffer(file, |text| text.len()).is_none());
}

#[test]
fn recursive_close_is_guarded_and_cancel_releases_the_guard() {
    let mut workspace = Workspace::empty().unwrap();
    let mut initial = workspace.begin_scratch(None, false).unwrap();
    drive(&mut workspace, &mut initial).unwrap();
    let window = workspace.active_window_id();
    let mut outer = workspace.begin_close_window(window, true).unwrap();
    assert!(matches!(
        workspace.advance(&mut outer, true).unwrap(),
        HostStep::WindowClose(_)
    ));
    let mut recursive = workspace.begin_close_window(window, true).unwrap();
    assert_eq!(
        drive(&mut workspace, &mut recursive).unwrap(),
        (Vec::new(), Outcome::Continue)
    );
    assert_eq!(workspace.window_count(), 1);
    workspace.cancel_plan(&outer);
    drop(outer);
    let mut retry = workspace.begin_close_window(window, true).unwrap();
    let (events, outcome) = drive(&mut workspace, &mut retry).unwrap();
    assert_eq!(outcome, Outcome::Quit);
    assert_eq!(events.len(), 2);
}

#[test]
fn window_close_callback_can_reopen_same_modified_buffer() {
    let dir = Directory::new();
    let path = dir.file("input", b"disk");
    let mut workspace = Workspace::empty().unwrap();
    let (file, window, _) = open(&mut workspace, &path);
    workspace
        .with_buffer_mut(file, |text| text.insert(0, b"unsaved ").unwrap())
        .unwrap();
    let mut close = workspace.begin_close_window(window, true).unwrap();
    assert!(matches!(
        workspace.advance(&mut close, true).unwrap(),
        HostStep::WindowClose(_)
    ));
    let (same, replacement, events) = open(&mut workspace, &path);
    assert_eq!(same, file);
    assert_eq!(events, vec![Event::WindowOpen(replacement)]);
    let (events, outcome) = drive(&mut workspace, &mut close).unwrap();
    assert_eq!(events, Vec::new(), "a remaining view prevents FileClose");
    assert_eq!(outcome, Outcome::Continue);
    assert_eq!(workspace.window_ids(), vec![replacement]);
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"unsaved disk".to_vec())
    );
    assert!(workspace.modified());
    assert_eq!(std::fs::read(path).unwrap(), b"disk");
}

#[test]
fn file_close_callback_can_reopen_before_the_old_window_is_invalidated() {
    let dir = Directory::new();
    let path = dir.file("input", b"disk");
    let mut workspace = Workspace::empty().unwrap();
    let (file, window, _) = open(&mut workspace, &path);
    let mut close = workspace.begin_close_window(window, true).unwrap();
    assert!(matches!(
        workspace.advance(&mut close, true).unwrap(),
        HostStep::WindowClose(_)
    ));
    assert!(matches!(
        workspace.advance(&mut close, true).unwrap(),
        HostStep::FileClose(_)
    ));
    let (same, replacement, events) = open(&mut workspace, &path);
    assert_eq!(same, file);
    assert_eq!(events, vec![Event::WindowOpen(replacement)]);
    drive(&mut workspace, &mut close).unwrap();
    assert_eq!(workspace.window_file_id(window), None);
    assert_eq!(workspace.window_file_id(replacement), Some(file));
    assert!(workspace.with_buffer(file, |text| text.len()).is_some());
}

#[test]
fn save_pre_growth_and_focus_change_keep_the_original_target() {
    for command in ["w", ",w"] {
        let dir = Directory::new();
        let first = dir.file("first", b"first");
        let second = dir.file("second", b"second");
        let mut workspace = Workspace::empty().unwrap();
        let (_, first_window, _) = open(&mut workspace, &first);
        let (second_file, _, _) = open(&mut workspace, &second);
        let mut save = workspace.begin_command(command).unwrap();
        match workspace.advance(&mut save, true).unwrap() {
            HostStep::SavePre(target) => {
                assert_eq!(target.buffer, second_file);
                assert_eq!(target.path, second);
            }
            _ => panic!("expected SavePre"),
        }
        workspace
            .with_buffer_mut(second_file, |text| {
                text.insert(text.len(), b" GROWN").unwrap()
            })
            .unwrap();
        workspace.focus_window(first_window).unwrap();
        match workspace.advance(&mut save, true).unwrap() {
            HostStep::SavePost { buffer, path } => {
                assert_eq!(buffer, second_file);
                assert_eq!(path, second);
            }
            _ => panic!("expected SavePost"),
        }
        assert_eq!(workspace.active_window_id(), first_window);
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second GROWN");
        assert_eq!(
            workspace.with_buffer(second_file, |text| text.modified()),
            Some(false)
        );
        drive(&mut workspace, &mut save).unwrap();
    }
}

#[test]
fn save_pre_veto_preserves_callback_edits_and_force_overrides_veto() {
    let dir = Directory::new();
    let path = dir.file("input", b"disk");
    let mut workspace = Workspace::empty().unwrap();
    let (file, _, _) = open(&mut workspace, &path);
    let mut save = workspace.begin_command("w").unwrap();
    assert!(matches!(
        workspace.advance(&mut save, true).unwrap(),
        HostStep::SavePre(_)
    ));
    workspace
        .with_buffer_mut(file, |text| text.insert(0, b"callback ").unwrap())
        .unwrap();
    assert!(workspace.advance(&mut save, false).is_err());
    workspace.cancel_plan(&save);
    assert_eq!(std::fs::read(&path).unwrap(), b"disk");
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"callback disk".to_vec())
    );
    assert!(workspace.modified());
    let mut forced = workspace.begin_command("w!").unwrap();
    assert!(
        matches!(workspace.advance(&mut forced, true).unwrap(), HostStep::SavePre(target) if target.force)
    );
    assert!(matches!(
        workspace.advance(&mut forced, false).unwrap(),
        HostStep::SavePost { .. }
    ));
    drive(&mut workspace, &mut forced).unwrap();
    assert_eq!(std::fs::read(path).unwrap(), b"callback disk");
}

#[test]
fn save_pre_closing_target_rejects_the_write_without_touching_other_buffer() {
    let dir = Directory::new();
    let first = dir.file("first", b"first");
    let second = dir.file("second", b"second");
    let mut workspace = Workspace::empty().unwrap();
    let (_, first_window, _) = open(&mut workspace, &first);
    let (_, second_window, _) = open(&mut workspace, &second);
    let mut save = workspace.begin_command("w").unwrap();
    assert!(matches!(
        workspace.advance(&mut save, true).unwrap(),
        HostStep::SavePre(_)
    ));
    let mut close = workspace.begin_close_window(second_window, true).unwrap();
    drive(&mut workspace, &mut close).unwrap();
    assert!(workspace.advance(&mut save, true).is_err());
    workspace.cancel_plan(&save);
    assert_eq!(workspace.active_window_id(), first_window);
    assert_eq!(std::fs::read(first).unwrap(), b"first");
    assert_eq!(std::fs::read(second).unwrap(), b"second");
}

#[test]
fn reload_replaces_the_file_handle_without_replacing_the_window() {
    let dir = Directory::new();
    let path = dir.file("input", b"before");
    let mut workspace = Workspace::empty().unwrap();
    let (old_file, window, _) = open(&mut workspace, &path);
    let original_selection = workspace.window_primary_selection(window);
    workspace.set_window_options(window, 3, true, true).unwrap();
    std::fs::write(&path, b"after reload").unwrap();
    let mut reload = workspace.begin_command("edit!").unwrap();
    let (events, _) = drive(&mut workspace, &mut reload).unwrap();
    assert_eq!(
        workspace.window_ids(),
        vec![window],
        "C :e! retains the same window object"
    );
    let new_file = workspace.window_file_id(window).unwrap();
    assert_ne!(new_file, old_file);
    assert_eq!(
        events,
        vec![Event::FileOpen(new_file), Event::FileClose(old_file)],
        "reload emits file events, not window events"
    );
    assert_eq!(workspace.window_options(window), Some((3, true, true)));
    assert_eq!(
        workspace.window_primary_selection(window),
        original_selection,
        "view_reload keeps the existing primary selection object"
    );
    assert!(workspace.with_buffer(old_file, |text| text.len()).is_none());
    assert_eq!(
        workspace.with_buffer(new_file, |text| text.to_vec()),
        Some(b"after reload".to_vec())
    );
}

#[test]
fn edit_callbacks_can_focus_and_open_windows_without_retargeting_the_close() {
    let dir = Directory::new();
    let first = dir.file("first", b"first");
    let second = dir.file("second", b"second");
    let third = dir.file("third", b"third");
    let mut workspace = Workspace::empty().unwrap();
    let (first_file, first_window, _) = open(&mut workspace, &first);
    let (third_file, third_window, _) = open(&mut workspace, &third);
    workspace.focus_window(first_window).unwrap();
    let mut edit = workspace
        .begin_command(&format!("edit '{}'", second.display()))
        .unwrap();
    let second_file = match workspace.advance(&mut edit, true).unwrap() {
        HostStep::FileOpen(file) => file,
        _ => panic!("edit must open its target file first"),
    };
    let second_window = match workspace.advance(&mut edit, true).unwrap() {
        HostStep::WindowOpen(window) => window,
        _ => panic!("edit must publish its new window before closing the old one"),
    };
    workspace.focus_window(third_window).unwrap();
    let mut callback_open = workspace.begin_scratch(None, false).unwrap();
    drive(&mut workspace, &mut callback_open).unwrap();
    let callback_window = workspace.active_window_id();
    let (events, _) = drive(&mut workspace, &mut edit).unwrap();
    assert_eq!(
        events,
        vec![
            Event::WindowClose(first_window),
            Event::FileClose(first_file)
        ]
    );
    assert_eq!(workspace.window_file_id(first_window), None);
    assert_eq!(workspace.window_file_id(second_window), Some(second_file));
    assert_eq!(workspace.window_file_id(third_window), Some(third_file));
    assert_eq!(workspace.active_window_id(), callback_window);
    assert_eq!(
        workspace.with_buffer(second_file, |text| text.to_vec()),
        Some(b"second".to_vec())
    );
    assert_eq!(
        workspace.with_buffer(third_file, |text| text.to_vec()),
        Some(b"third".to_vec())
    );
}

#[test]
fn reentrant_temporary_window_does_not_collect_an_outer_pending_file_open() {
    let dir = Directory::new();
    let path = dir.file("input", b"pending file contents");
    let mut workspace = Workspace::empty().unwrap();
    let mut outer = workspace.begin_open(&path, None, false).unwrap();
    let file = match workspace.advance(&mut outer, true).unwrap() {
        HostStep::FileOpen(file) => file,
        _ => panic!("expected FileOpen"),
    };
    let mut temporary = workspace.begin_scratch(None, false).unwrap();
    drive(&mut workspace, &mut temporary).unwrap();
    let window = workspace.active_window_id();
    let mut close = workspace.begin_close_window(window, true).unwrap();
    drive(&mut workspace, &mut close).unwrap();
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"pending file contents".to_vec())
    );
    let (events, outcome) = drive(&mut workspace, &mut outer).unwrap();
    assert!(matches!(events.as_slice(), [Event::WindowOpen(_)]));
    assert_eq!(outcome, Outcome::Continue);
    assert_eq!(workspace.window_count(), 1);
}

#[test]
fn cancelling_after_file_open_does_not_leave_an_unreferenced_buffer() {
    let dir = Directory::new();
    let path = dir.file("input", b"pending");
    let mut workspace = Workspace::empty().unwrap();
    let mut plan = workspace.begin_open(&path, None, false).unwrap();
    assert!(matches!(
        workspace.advance(&mut plan, true).unwrap(),
        HostStep::FileOpen(_)
    ));
    workspace.cancel_plan(&plan);
    drop(plan);
    assert!(workspace.window_ids().is_empty());
    assert!(workspace.file_ids().is_empty());
    let (_, _, events) = open(&mut workspace, &path);
    assert!(matches!(
        events.as_slice(),
        [Event::FileOpen(_), Event::WindowOpen(_)]
    ));
}

#[test]
fn opening_and_closing_a_reentrant_view_cannot_release_the_pending_outer_file() {
    let dir = Directory::new();
    let path = dir.file("input", b"retained");
    let mut workspace = Workspace::empty().unwrap();
    let mut outer = workspace.begin_open(&path, None, false).unwrap();
    let HostStep::FileOpen(file) = workspace.advance(&mut outer, true).unwrap() else {
        panic!("expected FileOpen")
    };
    let (_, temporary, _) = open(&mut workspace, &path);
    let mut close = workspace.begin_close_window(temporary, true).unwrap();
    drive(&mut workspace, &mut close).unwrap();
    assert_eq!(
        workspace.with_buffer(file, |text| text.to_vec()),
        Some(b"retained".to_vec())
    );
    drive(&mut workspace, &mut outer).unwrap();
    assert_eq!(workspace.window_count(), 1);
    assert_eq!(
        workspace.window_file_id(workspace.active_window_id()),
        Some(file)
    );
}

#[test]
fn changing_an_inactive_window_file_retains_identity_and_focus() {
    let dir = Directory::new();
    let first = dir.file("first", b"first");
    let second = dir.file("second", b"second");
    let third = dir.file("third", b"third");
    let mut workspace = Workspace::empty().unwrap();
    let (old, window, _) = open(&mut workspace, &first);
    let selection = workspace.window_primary_selection(window);
    let (_, focused, _) = open(&mut workspace, &second);
    let mut plan = workspace.begin_change_window_file(window, &third).unwrap();
    let (events, _) = drive(&mut workspace, &mut plan).unwrap();
    let new = workspace.window_file_id(window).unwrap();
    assert_eq!(workspace.active_window_id(), focused);
    assert_eq!(workspace.window_primary_selection(window), selection);
    assert_eq!(events, vec![Event::FileOpen(new), Event::FileClose(old)]);
    workspace.focus_window(window).unwrap();
    assert_eq!(workspace.editor.text.to_vec(), b"third");
}

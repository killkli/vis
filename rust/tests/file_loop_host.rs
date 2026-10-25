use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use vis::editor::{EditorAction, Mode};
use vis::workspace::{HostPlan, HostStep, Outcome, Workspace, command_rejected};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "vis-file-loops-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn bytes(ws: &Workspace, path: &Path) -> Vec<u8> {
    let file = ws
        .file_ids()
        .into_iter()
        .find(|file| {
            ws.buffer_path(*file)
                .and_then(|path| std::fs::canonicalize(path).ok())
                == std::fs::canonicalize(path).ok()
        })
        .unwrap();
    ws.with_buffer(file, |text| text.to_vec()).unwrap()
}

fn drive(ws: &mut Workspace, plan: &mut HostPlan) -> Outcome {
    loop {
        match ws.advance(plan, true).unwrap() {
            HostStep::Complete(outcome) => return outcome,
            HostStep::TargetCommand { token, request } => {
                let success = matches!(
                    ws.execute_target_command(&request),
                    Ok(EditorAction::Continue)
                );
                ws.resume_target_command(plan, token, success).unwrap();
            }
            HostStep::FileOpen(_)
            | HostStep::WindowOpen(_)
            | HostStep::WindowClose(_)
            | HostStep::FileClose(_)
            | HostStep::SavePre(_)
            | HostStep::SavePost { .. } => {}
            _ => panic!("unexpected host callback"),
        }
    }
}

#[test]
fn nested_structural_file_loops_edit_each_matching_file_without_focus_changes() {
    let directory = Directory::new();
    let a = directory.file("alpha", b"aa ba\n");
    let b = directory.file("beta", b"ab aa\n");
    let mut ws = Workspace::new(&[a.clone(), b.clone()], None, false).unwrap();
    let focused = ws.active_window_id();
    ws.command("X ,x/a/ {i/</ a/>/}").unwrap();
    assert_eq!(ws.active_window_id(), focused);
    assert_eq!(bytes(&ws, &a), b"<a><a> b<a>\n");
    assert_eq!(bytes(&ws, &b), b"<a>b <a><a>\n");
    ws.command("Y/alpha$/ ,c/unmatched/").unwrap();
    assert_eq!(bytes(&ws, &a), b"<a><a> b<a>\n");
    assert_eq!(bytes(&ws, &b), b"unmatched");
    ws.command("X/alpha$/ X/beta$/ ,c/nested/").unwrap();
    assert_eq!(bytes(&ws, &b), b"nested");
}

#[test]
fn window_duplicates_share_one_raw_coordinate_transcript_and_one_undo() {
    let directory = Directory::new();
    let path = directory.file("shared", b"text\n");
    let mut ws = Workspace::new(std::slice::from_ref(&path), None, false).unwrap();
    ws.command("split").unwrap();
    let error = ws.command("X ,c/conflict/").unwrap_err();
    assert!(command_rejected(&error));
    assert_eq!(bytes(&ws, &path), b"text\n");
    ws.command("X i/+/ ").unwrap();
    assert_eq!(bytes(&ws, &path), b"++text\n");
    ws.handle_key(vis::editor::Key::Char('u')).unwrap();
    assert_eq!(bytes(&ws, &path), b"text\n");
}

#[test]
fn structural_failure_cancels_all_queued_file_text_in_strict_and_compatible_modes() {
    for compatible in [false, true] {
        for failing in ["r /this/vis/file/does/not/exist", "| false"] {
            let directory = Directory::new();
            let a = directory.file("alpha", b"one\n");
            let b = directory.file("beta", b"two\n");
            let mut ws = Workspace::new(&[a.clone(), b.clone()], None, false).unwrap();
            let command = format!("X ,{{i/queued/\n{failing}\n}}");
            if compatible {
                let mut plan = ws.prepare_command(&command).unwrap();
                drive(&mut ws, &mut plan);
            } else {
                assert!(ws.command(&command).is_err());
            }
            assert_eq!(bytes(&ws, &a), b"one\n");
            assert_eq!(bytes(&ws, &b), b"two\n");
        }
    }
}

#[test]
fn save_callbacks_keep_original_target_and_global_focus_without_early_text_commit() {
    let directory = Directory::new();
    let a = directory.file("alpha", b"one\n");
    let b = directory.file("beta", b"two\n");
    let mut ws = Workspace::new(&[a.clone(), b.clone()], None, false).unwrap();
    let focused = ws.active_window_id();
    let mut plan = ws.begin_command("X w").unwrap();
    let mut saved = Vec::new();
    loop {
        match ws.advance(&mut plan, true).unwrap() {
            HostStep::SavePre(save) => {
                assert_eq!(ws.active_window_id(), focused);
                let size = ws.with_buffer(save.buffer, |text| text.len()).unwrap();
                ws.with_buffer_mut(save.buffer, |text| text.insert(size, b"growth"))
                    .unwrap()
                    .unwrap();
                ws.focus_window(focused).unwrap();
            }
            HostStep::SavePost { buffer, path } => saved.push((buffer, path)),
            HostStep::Complete(_) => break,
            _ => panic!("unexpected file-loop save callback"),
        }
    }
    assert_eq!(saved.len(), 2);
    assert_eq!(std::fs::read(a).unwrap(), b"one\ngrowth");
    assert_eq!(std::fs::read(b).unwrap(), b"two\ngrowth");
    assert_eq!(ws.active_window_id(), focused);
}

#[test]
fn destructive_file_loop_drains_after_closing_the_origin_and_obeys_unsaved_protection() {
    let directory = Directory::new();
    let a = directory.file("alpha", b"one\n");
    let b = directory.file("beta", b"two\n");
    let mut ws = Workspace::new(&[a, b], None, false).unwrap();
    assert_eq!(ws.command("X q").unwrap(), Outcome::Quit);
    assert_eq!(ws.window_count(), 0);
    let mut ws = Workspace::new(&[], None, false).unwrap();
    ws.handle_key(vis::editor::Key::Char('i')).unwrap();
    ws.handle_key(vis::editor::Key::Char('X')).unwrap();
    ws.handle_key(vis::editor::Key::Escape).unwrap();
    let mut plan = ws.prepare_command("X q").unwrap();
    drive(&mut ws, &mut plan);
    assert_eq!(ws.window_count(), 1);
    assert_eq!(ws.editor.text.to_vec(), b"X");
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.command("X q!").unwrap(), Outcome::Quit);
}

#[test]
fn destructive_commands_are_validated_before_any_group_or_extraction_effect() {
    let mut ws = Workspace::new(&[], Some(b"text\n".to_vec()), false).unwrap();
    for command in ["X {i/queued/ q!}", "X ,x/./ q!", "{X q!}"] {
        let error = ws.command(command).unwrap_err();
        assert!(command_rejected(&error));
        assert_eq!(ws.window_count(), 1);
        assert_eq!(ws.editor.text.to_vec(), b"text\n");
    }
}

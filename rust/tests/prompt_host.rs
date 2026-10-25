use vis::editor::{Key, Mode};
use vis::selection::Selection;
use vis::workspace::{HostStep, Outcome, Workspace};

fn workspace(bytes: &[u8]) -> Workspace {
    let mut ws = Workspace::new(&[], Some(bytes.to_vec()), false).unwrap();
    let file = ws.window_file_id(ws.active_window_id()).unwrap();
    ws.with_buffer_mut(file, vis::text::Text::mark_saved)
        .unwrap();
    ws
}

fn keys(workspace: &mut Workspace, input: &str) -> Outcome {
    let keys = workspace.editor.parse_keys(input).unwrap();
    let mut outcome = Outcome::Continue;
    for key in keys {
        outcome = workspace.handle_key(key).unwrap();
    }
    outcome
}

#[test]
fn prompt_is_an_internal_window_with_ordinary_modes_and_parent_identity() {
    let mut ws = workspace(b"document\n");
    let parent = ws.active_window_id();
    let file = ws.window_file_id(parent).unwrap();
    keys(&mut ws, ":pwd");
    let prompt = ws.active_window_id();
    let history = ws.window_file_id(prompt).unwrap();
    assert_ne!(prompt, parent);
    assert_ne!(history, file);
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert_eq!(ws.editor.text.to_vec(), b":pwd\n");
    assert_eq!(ws.editor.cursor, 4);
    assert_eq!(ws.window_internal(prompt), Some(true));
    assert_eq!(ws.external_window_ids(), [parent]);
    assert_eq!(ws.file_ids(), [file]);
    assert!(!ws.modified());
    let display = ws.window_display(prompt).unwrap();
    assert!(display.oneline && !display.statusbar && !display.showeof);
    keys(&mut ws, "<Escape>");
    assert_eq!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.mode, Mode::Normal);
    keys(&mut ws, "<Escape>");
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.text.to_vec(), b"document\n");
    assert_eq!(ws.window_file_id(prompt), None);
    assert_eq!(
        ws.with_buffer(history, |text| text.to_vec()).unwrap(),
        b":pwd\n"
    );
    keys(&mut ws, ":");
    assert_eq!(ws.window_file_id(ws.active_window_id()), Some(history));
    assert_ne!(ws.active_window_id(), prompt);
}

#[test]
fn normal_prompt_editing_uses_the_real_cursor_and_preserves_document_register() {
    let mut ws = workspace(b"one two\n");
    keys(&mut ws, "yiw");
    let register = ws.editor.register('"').unwrap().bytes.clone();
    keys(&mut ws, ":pxd<Escape>0llxiw");
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert_eq!(ws.editor.text.to_vec(), b":pwd\n");
    assert_eq!(ws.editor.register('"').unwrap().bytes, register);
    keys(&mut ws, "<Enter>");
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.text.to_vec(), b"one two\n");
    assert_eq!(ws.editor.register(':').unwrap().bytes, b"pwd");
}

#[test]
fn mixed_prompt_history_expands_and_reexecuted_line_moves_to_end() {
    let mut ws = workspace(b"target\n");
    keys(&mut ws, ":pwd<Enter>/target<Enter>:pwd<Enter>:");
    let prompt = ws.active_window_id();
    let history = ws.window_file_id(prompt).unwrap();
    assert_eq!(ws.editor.text.to_vec(), b":pwd\n/target\n:pwd\n:\n");
    keys(&mut ws, "<Up>");
    assert_eq!(ws.editor.mode, Mode::Insert);
    let display = ws.window_display(prompt).unwrap();
    assert!(!display.oneline && !display.statusbar && display.showeof);
    keys(&mut ws, "<Escape>k<Enter>");
    assert_eq!(ws.window_file_id(prompt), None);
    assert_eq!(ws.editor.text.to_vec(), b"target\n");
    assert_eq!(
        ws.with_buffer(history, |t| t.to_vec()).unwrap(),
        b":pwd\n:pwd\n/target\n"
    );
}

#[test]
fn command_failure_retains_same_prompt_then_can_be_edited_and_retried() {
    let mut ws = workspace(b"unchanged\n");
    let parent = ws.active_window_id();
    keys(&mut ws, ":unimplemented");
    let prompt = ws.active_window_id();
    let result = keys(&mut ws, "<Enter>");
    assert!(matches!(result, Outcome::Message(_)));
    assert_eq!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert_eq!(ws.editor.text.to_vec(), b":unimplemented\n");
    keys(&mut ws, "<C-u>:pwd<Enter>");
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.text.to_vec(), b"unchanged\n");
}

#[test]
fn invalid_search_stays_open_but_missing_valid_pattern_closes() {
    let mut ws = workspace(b"one two one\n");
    let parent = ws.active_window_id();
    keys(&mut ws, "/one<Enter>");
    assert_eq!(ws.editor.cursor, 8);
    keys(&mut ws, "/<Enter>");
    assert_eq!(ws.editor.cursor, 8, "bare search does not repeat");
    keys(&mut ws, "/[");
    let prompt = ws.active_window_id();
    keys(&mut ws, "<Enter>");
    assert_eq!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.mode, Mode::Insert);
    keys(&mut ws, "<Backspace>missing<Enter>");
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.cursor, 8);
    assert_eq!(ws.editor.register('/').unwrap().bytes, b"missing");
}

#[test]
fn visual_parent_and_pending_operator_survive_prompt_swap() {
    let mut ws = workspace(b"a b c b d\n");
    keys(&mut ws, "2d/b<Enter>");
    assert_eq!(ws.editor.text.to_vec(), b"b d\n");
    keys(&mut ws, "v:");
    let prompt = ws.active_window_id();
    keys(&mut ws, "<Escape><Escape>");
    assert_ne!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.mode, Mode::Visual);
    assert!(
        ws.editor
            .selection_anchored(ws.editor.selection_ids()[0])
            .unwrap()
    );
}

#[test]
fn normal_escape_disposes_secondary_prompt_selections_before_closing() {
    let mut ws = workspace(b"document");
    keys(&mut ws, ":pwd<Escape>");
    let prompt = ws.active_window_id();
    ws.editor
        .create_selection(
            Selection {
                anchor: 0,
                cursor: 0,
            },
            false,
        )
        .unwrap();
    keys(&mut ws, "<Escape>");
    assert_eq!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.selections().len(), 1);
    keys(&mut ws, "<Escape>");
    assert_ne!(ws.active_window_id(), prompt);
}

#[test]
fn typed_command_continuation_observes_parent_then_retains_failed_prompt() {
    let mut ws = workspace(b"document\n");
    let parent = ws.active_window_id();
    keys(&mut ws, ":custom");
    let prompt = ws.active_window_id();
    let mut plan = ws.begin_key(Key::Enter).unwrap();
    let token = match ws.advance(&mut plan, true).unwrap() {
        HostStep::PromptCommand { token, command } => {
            assert_eq!(command, "custom");
            token
        }
        _ => panic!("expected prompt command"),
    };
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert!(
        ws.frames()
            .iter()
            .find(|f| f.id == parent)
            .unwrap()
            .prompt_executing
    );
    ws.resume_prompt_command(&mut plan, token, false).unwrap();
    assert!(matches!(
        ws.advance(&mut plan, true).unwrap(),
        HostStep::Complete(_)
    ));
    assert_eq!(ws.active_window_id(), prompt);
    assert_eq!(ws.editor.mode, Mode::Insert);
    assert!(!ws.frames().iter().any(|f| f.prompt_executing));
}

#[test]
fn internals_exist_before_files_open_and_empty_drafts_do_not_pollute_history() {
    let empty = Workspace::empty().unwrap();
    assert_eq!(empty.all_file_ids().len(), 2);
    assert!(empty.file_ids().is_empty());
    assert!(
        empty
            .all_file_ids()
            .into_iter()
            .all(|id| empty.buffer_internal(id) == Some(true))
    );
    let mut ws = workspace(b"text");
    keys(&mut ws, ":pwd<Enter>:");
    let history = ws.window_file_id(ws.active_window_id()).unwrap();
    keys(&mut ws, "<Up><Enter>");
    assert_eq!(ws.with_buffer(history, |t| t.to_vec()).unwrap(), b":pwd\n");
    keys(&mut ws, ":<Escape><Escape>/<Enter>?<Escape><Escape>");
    assert_eq!(ws.with_buffer(history, |t| t.to_vec()).unwrap(), b":pwd\n");
    assert!(!ws.modified());
}

#[test]
fn accepted_save_handler_failure_hides_prompt_without_saving() {
    let mut ws = workspace(b"text\n");
    let parent = ws.active_window_id();
    assert!(
        ws.command("w").is_err(),
        "strict Rust API retains write errors"
    );
    keys(&mut ws, ":w");
    let prompt = ws.active_window_id();
    assert!(matches!(keys(&mut ws, "<Enter>"), Outcome::Message(_)));
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.window_file_id(prompt), None);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.text.to_vec(), b"text\n");
    assert_eq!(keys(&mut ws, ":q!<Enter>"), Outcome::Quit);
    assert_eq!(ws.window_count(), 0);
    assert_eq!(ws.all_file_ids().len(), 2);
}

#[test]
fn accepted_quit_handler_failure_hides_prompt_and_preserves_unsaved_document() {
    let mut ws = workspace(b"text\n");
    let parent = ws.active_window_id();
    keys(&mut ws, "iX<Escape>");
    assert!(ws.command("q").is_err());
    let result = keys(&mut ws, ":q<Enter>");
    assert!(matches!(result, Outcome::Message(_)));
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.window_count(), 1);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.text.to_vec(), b"Xtext\n");
    assert!(ws.modified());
}

#[test]
fn native_command_acceptance_distinguishes_evaluation_from_unknown_syntax() {
    let mut ws = workspace(b"text\n");
    let parent = ws.active_window_id();
    for command in ["map! insert jj", "'ap", "1 | false"] {
        assert!(ws.command(command).is_err(), "strict command: {command}");
        let input = format!(":{command}<Enter>");
        keys(&mut ws, &input);
        assert_eq!(ws.active_window_id(), parent, "accepted command: {command}");
        assert_eq!(ws.editor.mode, Mode::Normal);
        assert_eq!(ws.editor.text.to_vec(), b"text\n");
    }
    keys(&mut ws, ":this-command-is-unknown<Enter>");
    assert_ne!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.mode, Mode::Insert);
}

#[test]
fn preparation_rejects_transcript_conflicts_and_preserves_atomic_text() {
    let mut ws = workspace(b"original\n");
    assert!(ws.prepare_command("{c/first/ c/second/}").is_err());
    assert_eq!(ws.editor.text.to_vec(), b"original\n");
    assert!(ws.prepare_command("this-command-is-unknown").is_err());
    assert_eq!(ws.editor.text.to_vec(), b"original\n");
}

#[test]
fn chained_search_prompts_reuse_parent_with_latest_cursor() {
    let mut ws = workspace(b"abc xyz\n");
    let parent = ws.active_window_id();
    keys(&mut ws, ":set expandtab<Enter>/xyz<Enter>?abc<Enter>");
    assert_eq!(ws.active_window_id(), parent);
    assert_eq!(ws.editor.cursor, 0);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert!(ws.editor.expand_tab);
}

#[test]
fn native_host_drains_mapping_keys_across_prompt_host_requests() {
    let mut ws = workspace(b"document\n");
    ws.editor
        .map_alias(Mode::Normal, ";", ":pwd<Enter>iX<Escape>", None)
        .unwrap();
    keys(&mut ws, ";");
    assert_eq!(ws.window_count(), 1);
    assert_eq!(ws.editor.mode, Mode::Normal);
    assert_eq!(ws.editor.text.to_vec(), b"Xdocument\n");
}

#[test]
fn qall_ignores_internal_history_changes_and_exits_with_message_window_open() {
    let mut ws = workspace(b"clean\n");
    let message = ws.begin_message(b"diagnostic".to_vec()).unwrap();
    // Drive the message lifecycle without Lua, then return to the document.
    let mut message = message;
    while !matches!(
        ws.advance(&mut message, true).unwrap(),
        HostStep::Complete(_)
    ) {}
    let parent = ws.external_window_ids()[0];
    ws.focus_window(parent).unwrap();
    assert_eq!(keys(&mut ws, ":qall<Enter>"), Outcome::Quit);
    assert!(ws.external_window_ids().is_empty());
}

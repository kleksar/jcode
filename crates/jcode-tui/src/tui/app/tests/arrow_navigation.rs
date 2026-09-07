#[test]
fn plain_arrows_traverse_sessions_chat_diff_and_files_without_wrapping() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();
    let mut app = create_test_app();

    // Chat -> Sessions -> Chat. Sessions owns no-op Left and consumes Right
    // as its explicit return-to-chat boundary.
    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("open sessions");
    assert!(app.session_picker_overlay.is_some());
    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("sessions left boundary");
    assert!(app.session_picker_overlay.is_some());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("return to chat");
    assert!(app.session_picker_overlay.is_none());

    // Chat -> Diff -> Files, then return left through the ordered views.
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("open diff");
    assert!(app.diff_pane_focus);
    assert!(!app.worktree_files_tab_active());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("open files");
    assert!(app.diff_pane_focus);
    assert!(app.worktree_files_tab_active());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("files right boundary");
    assert!(app.worktree_files_tab_active());
    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("files to diff");
    assert!(!app.worktree_files_tab_active());
    assert!(app.diff_pane_focus);
    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("diff to chat");
    assert!(!app.diff_pane_focus);
}

#[test]
fn nonempty_composer_keeps_plain_arrow_cursor_navigation() {
    let mut app = create_test_app();
    app.input = "hello".to_string();
    app.cursor_pos = 2;

    app.handle_key(KeyCode::Left, KeyModifiers::NONE)
        .expect("move cursor left");
    assert_eq!(app.cursor_pos, 1);
    assert!(app.session_picker_overlay.is_none());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("move cursor right");
    assert_eq!(app.cursor_pos, 2);
    assert!(!app.diff_pane_focus);
}

#[test]
fn non_session_overlays_keep_plain_arrow_ownership() {
    let mut app = create_test_app();
    app.help_scroll = Some(0);

    app.handle_key(KeyCode::Right, KeyModifiers::NONE)
        .expect("route right to help overlay");
    assert_eq!(app.help_scroll, Some(0));
    assert!(!app.diff_pane_focus);
}

#[test]
fn remote_plain_arrows_traverse_views_and_keep_composer_and_overlay_guards() {
    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();
    let mut app = create_test_app();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    runtime
        .block_on(app.handle_remote_key(KeyCode::Left, KeyModifiers::NONE, &mut remote))
        .expect("remote chat to sessions");
    assert!(app.session_picker_overlay.is_some());
    runtime
        .block_on(app.handle_remote_key(KeyCode::Left, KeyModifiers::NONE, &mut remote))
        .expect("remote sessions left boundary");
    assert!(app.session_picker_overlay.is_some());
    runtime
        .block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .expect("remote sessions to chat");
    assert!(app.session_picker_overlay.is_none());

    for (key, files_active) in [
        (KeyCode::Right, false),
        (KeyCode::Right, true),
        (KeyCode::Right, true),
        (KeyCode::Left, false),
    ] {
        runtime
            .block_on(app.handle_remote_key(key, KeyModifiers::NONE, &mut remote))
            .expect("remote workspace transition");
        assert_eq!(app.worktree_files_tab_active(), files_active);
    }
    assert!(app.diff_pane_focus);
    runtime
        .block_on(app.handle_remote_key(KeyCode::Left, KeyModifiers::NONE, &mut remote))
        .expect("remote diff to chat");
    assert!(!app.diff_pane_focus);

    app.input = "hello".to_string();
    app.cursor_pos = 2;
    runtime
        .block_on(app.handle_remote_key(KeyCode::Left, KeyModifiers::NONE, &mut remote))
        .expect("remote cursor left");
    assert_eq!(app.cursor_pos, 1);
    runtime
        .block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .expect("remote cursor right");
    assert_eq!(app.cursor_pos, 2);
    assert!(app.session_picker_overlay.is_none());
    assert!(!app.diff_pane_focus);

    app.input.clear();
    app.help_scroll = Some(0);
    runtime
        .block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .expect("remote overlay right");
    assert_eq!(app.help_scroll, Some(0));
    assert!(!app.diff_pane_focus);
}

#[test]
fn remote_key_event_wrapper_decodes_plain_workspace_arrows() {
    use crossterm::event::{KeyEvent, KeyEventKind};

    let runtime = tokio::runtime::Runtime::new().expect("test runtime");
    let _guard = runtime.enter();
    let mut app = create_test_app();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    let press = |app: &mut App, key, remote: &mut crate::tui::backend::RemoteConnection| {
        runtime.block_on(remote::handle_remote_key_event(
            app,
            KeyEvent::new_with_kind(key, KeyModifiers::NONE, KeyEventKind::Press),
            remote,
        ))
    };

    press(&mut app, KeyCode::Left, &mut remote).expect("event opens sessions");
    assert!(app.session_picker_overlay.is_some());
    press(&mut app, KeyCode::Right, &mut remote).expect("event returns chat");
    assert!(app.session_picker_overlay.is_none());
    press(&mut app, KeyCode::Right, &mut remote).expect("event opens diff");
    assert!(app.diff_pane_focus);
    assert!(!app.worktree_files_tab_active());
    press(&mut app, KeyCode::Right, &mut remote).expect("event opens files");
    assert!(app.worktree_files_tab_active());
    press(&mut app, KeyCode::Left, &mut remote).expect("event returns diff");
    assert!(!app.worktree_files_tab_active());
}

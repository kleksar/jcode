#[test]
fn test_mouse_scroll_over_tool_side_panel_keeps_typing_in_chat() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.diff_pane_scroll = 5;
    app.diff_pane_focus = false;
    app.diff_pane_auto_scroll = true;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    crate::tui::ui::record_layout_snapshot(
        Rect::new(0, 0, 40, 20),
        None,
        Some(Rect::new(40, 0, 20, 20)),
        None,
    );

    let scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 45,
        row: 5,
        modifiers: KeyModifiers::empty(),
    });
    assert!(
        !scroll_only,
        "side-panel wheel scroll should still keep chat focus while redrawing immediately"
    );
    assert!(!app.diff_pane_focus);

    app.handle_key(KeyCode::Char('x'), KeyModifiers::empty())
        .expect("typing into chat should succeed");

    assert_eq!(app.input, "x");
}

#[test]
fn test_mouse_scroll_over_tool_side_panel_updates_visible_render() {
    let _lock = scroll_render_test_lock();

    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.diff_pane_scroll = 0;
    app.diff_pane_focus = false;
    app.diff_pane_auto_scroll = true;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: (1..=30)
                .map(|i| format!("- side-scroll-{i:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
            updated_at_ms: 1,
        }],
    };

    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");

    let before = render_and_snap(&app, &mut terminal);
    assert!(crate::tui::ui::pinned_pane_total_lines() > 3);
    let diff_area = crate::tui::ui::last_layout_snapshot()
        .and_then(|l| l.diff_pane_area)
        .expect("expected side panel area after render");
    assert!(before.contains("side-scroll-01"));

    let _scroll_only = app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: diff_area.x + diff_area.width / 2,
        row: diff_area.y + diff_area.height.saturating_sub(2).min(3),
        modifiers: KeyModifiers::empty(),
    });
    assert_eq!(app.diff_pane_scroll, 3);

    let after = render_and_snap(&app, &mut terminal);
    assert_eq!(crate::tui::ui::last_diff_pane_effective_scroll(), 3);
    assert_ne!(
        before, after,
        "hover scrolling should repaint the side panel"
    );
    assert!(after.contains("side-scroll-04"));
    assert!(after.contains("side-scroll-05"));
    assert!(!after.contains("side-scroll-01"));
}

#[test]
fn test_tool_side_panel_uses_shared_right_pane_keyboard_focus() {
    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "hello".to_string(),
            updated_at_ms: 1,
        }],
    };

    assert!(app.diff_pane_visible());
    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));
    assert!(app.diff_pane_focus);

    // Cycle the diff display mode via its configured chord (Alt+G by
    // default; BackTab was remapped to model-favorite cycling).
    assert!(super::input::handle_navigation_shortcuts(
        &mut app,
        KeyCode::Char('g'),
        KeyModifiers::ALT
    ));
    assert!(
        app.diff_pane_focus,
        "cycling diff display should not drop focus when tool side panel is still visible"
    );
}

#[test]
fn test_side_panel_uses_left_splitter_instead_of_rounded_box() {
    let _lock = scroll_render_test_lock();

    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "alpha\nbeta\ngamma".to_string(),
            updated_at_ms: 1,
        }],
    };

    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let text = render_and_snap(&app, &mut terminal);

    let diff_area = crate::tui::ui::last_layout_snapshot()
        .and_then(|layout| layout.diff_pane_area)
        .expect("expected side panel area after render");
    let buf = terminal.backend().buffer();

    assert_eq!(buf[(diff_area.x, diff_area.y)].symbol(), "│");
    assert_eq!(buf[(diff_area.x, diff_area.y + 1)].symbol(), "│");
    assert!(text.contains("side Plan 1/1"), "rendered text: {text}");
}

#[test]
fn test_pinned_content_uses_left_splitter_instead_of_rounded_box() {
    let _lock = scroll_render_test_lock();

    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::Pinned;
    app.display_messages = vec![DisplayMessage {
        role: "tool".to_string(),
        content: "wrote src/demo.rs".to_string(),
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: Some(crate::message::ToolCall {
            id: "tool-1".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({
                "file_path": "src/demo.rs",
                "content": "fn demo() {}\n"
            }),
            intent: None, thought_signature: None, }),
    }];
    app.bump_display_messages_version();

    let backend = ratatui::backend::TestBackend::new(80, 12);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let text = render_and_snap(&app, &mut terminal);

    let diff_area = crate::tui::ui::last_layout_snapshot()
        .and_then(|layout| layout.diff_pane_area)
        .expect("expected pinned pane area after render");
    let buf = terminal.backend().buffer();

    assert_eq!(buf[(diff_area.x, diff_area.y)].symbol(), "│");
    assert_eq!(buf[(diff_area.x, diff_area.y + 1)].symbol(), "│");
    assert!(
        text.contains("side Pinned +1 -0 1f"),
        "rendered text: {text}"
    );
}

#[test]
fn test_file_diff_uses_left_splitter_instead_of_rounded_box() {
    let _lock = scroll_render_test_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let file_path = temp.path().join("demo.rs");
    std::fs::write(&file_path, "fn demo() {}\n").expect("write demo file");

    let mut app = create_test_app();
    app.diff_mode = crate::config::DiffDisplayMode::File;
    app.display_messages = vec![DisplayMessage {
        role: "tool".to_string(),
        content: "updated demo.rs".to_string(),
        tool_calls: vec![],
        duration_secs: None,
        title: None,
        tool_data: Some(crate::message::ToolCall {
            id: "tool-1".to_string(),
            name: "write".to_string(),
            input: serde_json::json!({
                "file_path": file_path.display().to_string(),
                "content": "fn demo() {\n    println!(\"hi\");\n}\n"
            }),
            intent: None, thought_signature: None, }),
    }];
    app.bump_display_messages_version();

    let backend = ratatui::backend::TestBackend::new(100, 18);
    let mut terminal = ratatui::Terminal::new(backend).expect("failed to create test terminal");
    let text = render_and_snap(&app, &mut terminal);

    let diff_area = crate::tui::ui::last_layout_snapshot()
        .and_then(|layout| layout.diff_pane_area)
        .expect("expected file diff pane area after render");
    let buf = terminal.backend().buffer();

    assert_eq!(buf[(diff_area.x, diff_area.y)].symbol(), "│");
    assert_eq!(buf[(diff_area.x, diff_area.y + 1)].symbol(), "│");
    assert!(text.contains("demo.rs"), "rendered text: {text}");
}

fn init_worktree_pane_test_repo() -> tempfile::TempDir {
    let repo = tempfile::tempdir().expect("temp repo");
    let git = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .current_dir(repo.path())
            .args(args)
            .status()
            .expect("run git");
        assert!(status.success(), "git command failed: {args:?}");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "jcode-test@example.invalid"]);
    git(&["config", "user.name", "Jcode Test"]);
    std::fs::write(repo.path().join("demo.rs"), "fn old() {}\n").expect("write baseline");
    git(&["add", "demo.rs"]);
    git(&["commit", "-qm", "baseline"]);
    std::fs::write(repo.path().join("demo.rs"), "fn new() {}\n").expect("write edit");
    std::fs::write(repo.path().join("notes.txt"), "first\nsecond\n")
        .expect("write untracked file");
    repo
}

#[test]
fn test_wide_dirty_worktree_renders_automatic_changes_pane() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let backend = ratatui::backend::TestBackend::new(140, 24);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");
    let pane = layout.diff_pane_area.expect("automatic worktree pane");

    assert!(pane.width >= 42, "pane should remain readable: {pane:?}");
    assert!(layout.messages_area.width >= 56, "chat should remain usable: {layout:?}");
    assert!(text.contains("changes 2 files +3 -1"), "rendered text: {text}");
    assert!(text.contains("demo.rs"), "rendered text: {text}");
    assert!(text.contains("notes.txt"), "rendered text: {text}");
    assert!(text.contains("fn new() {}"), "rendered text: {text}");
    assert!(text.contains("fn old() {}"), "rendered text: {text}");
    assert!(app.diff_pane_visible());
    assert!(app.handle_diagram_ctrl_key(KeyCode::Char('l'), false));
    assert!(app.diff_pane_focus, "Ctrl+L should focus the changes pane");
}

#[test]
fn test_automatic_worktree_pane_hides_in_narrow_terminal() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;

    let backend = ratatui::backend::TestBackend::new(109, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");

    assert!(layout.diff_pane_area.is_none(), "narrow layout: {layout:?}");
    assert!(!text.contains("changes 2 files"), "rendered text: {text}");
}

#[test]
fn test_automatic_worktree_pane_hides_for_clean_repo() {
    let _lock = scroll_render_test_lock();
    let repo = tempfile::tempdir().expect("temp repo");
    let status = std::process::Command::new("git")
        .current_dir(repo.path())
        .args(["init", "-q"])
        .status()
        .expect("git init");
    assert!(status.success());
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());

    let backend = ratatui::backend::TestBackend::new(140, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::last_layout_snapshot().expect("layout snapshot");

    assert!(layout.diff_pane_area.is_none(), "clean layout: {layout:?}");
    assert!(!text.contains(" changes "), "rendered text: {text}");
}

#[test]
fn test_explicit_side_panel_takes_precedence_over_worktree_changes() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());

    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".to_string()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            file_path: "".to_string(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "explicit-panel-content".to_string(),
            updated_at_ms: 1,
        }],
    };

    let backend = ratatui::backend::TestBackend::new(140, 20);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let text = render_and_snap(&app, &mut terminal);

    assert!(text.contains("explicit-panel-content"), "rendered text: {text}");
    assert!(!text.contains("changes 2 files"), "rendered text: {text}");
}

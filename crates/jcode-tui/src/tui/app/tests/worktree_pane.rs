fn worktree_test_mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn worktree_filter_fixture() -> (
    tempfile::TempDir,
    App,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let list = crate::tui::ui::worktree_pane_layout().unwrap().list_area;
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        list.x + 1,
        list.y,
    ));
    render_and_snap(&app, &mut terminal);
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
    (repo, app, terminal)
}

#[test]
fn test_worktree_header_has_padded_heavy_boundary() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();

    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("worktree pane layout");
    assert!(
        layout.body_area.y >= layout.list_area.bottom().saturating_add(3),
        "hint, heavy rule, and padding should precede diff body: list={:?} body={:?}",
        layout.list_area,
        layout.body_area
    );

    let rule_y = layout.body_area.y - 2;
    let padding_y = layout.body_area.y - 1;
    let buffer = terminal.backend().buffer();
    assert!(
        (layout.body_area.x..layout.body_area.right()).all(|x| buffer[(x, rule_y)].symbol() == "━"),
        "heavy rule should span the diff body width"
    );
    assert!(
        (layout.body_area.x..layout.body_area.right())
            .all(|x| buffer[(x, padding_y)].symbol().trim().is_empty()),
        "blank padding row should separate the rule from diff content"
    );
}

#[test]
fn test_files_command_opens_project_tree_with_inline_preview() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    assert!(super::commands::handle_files_command(&mut app, "/files"));

    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("files pane layout");

    assert!(layout.files_tab_active, "Files tab should be active: {text}");
    assert!(text.contains("Diff") && text.contains("Files"), "tab header: {text}");
    assert!(text.contains("demo.rs"), "project tree: {text}");
    assert!(text.contains("fn new() {}"), "selected file preview: {text}");
    assert!(layout.tree_area.is_some());
    assert!(layout.preview_area.is_some());
}

#[test]
fn test_files_tree_expands_and_tab_returns_to_diff() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::create_dir_all(repo.path().join("src/nested")).expect("create source tree");
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn library() {}\n").expect("write lib");
    std::fs::write(repo.path().join("src/nested/mod.rs"), "pub mod leaf;\n").expect("write mod");
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.open_project_files_pane();

    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
    let collapsed = render_and_snap(&app, &mut terminal);
    assert!(collapsed.contains("▸ src"), "collapsed tree: {collapsed}");
    assert!(!collapsed.contains("lib.rs"), "children start hidden: {collapsed}");

    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    let expanded = render_and_snap(&app, &mut terminal);
    assert!(expanded.contains("▾ src"), "expanded tree: {expanded}");
    assert!(expanded.contains("lib.rs"), "expanded child: {expanded}");

    assert!(app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE));
    let diff = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("diff pane layout");
    assert!(!layout.files_tab_active, "Tab should switch to Diff: {diff}");
    assert!(diff.contains("changes"), "dirty diff should render after switch: {diff}");
}

#[test]
fn test_files_header_tabs_switch_with_mouse() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

    render_and_snap(&app, &mut terminal);
    let diff_layout = crate::tui::ui::worktree_pane_layout().expect("automatic diff pane");
    assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        diff_layout.files_tab_area.x + 1,
        diff_layout.files_tab_area.y,
    )));
    render_and_snap(&app, &mut terminal);
    let files_layout = crate::tui::ui::worktree_pane_layout().expect("files pane");
    assert!(files_layout.files_tab_active);

    assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        files_layout.diff_tab_area.x + 1,
        files_layout.diff_tab_area.y,
    )));
    render_and_snap(&app, &mut terminal);
    assert!(!crate::tui::ui::worktree_pane_layout()
        .expect("diff pane")
        .files_tab_active);
}

#[test]
fn test_files_pane_opens_and_closes_in_clean_repo() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .status()
                .expect("run git")
                .success()
        );
    };
    git(&["add", "-A"]);
    git(&["commit", "-qm", "clean fixture"]);
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

    assert!(super::commands::handle_files_command(&mut app, "/files"));
    let open = render_and_snap(&app, &mut terminal);
    assert!(open.contains("Files"), "clean project explorer: {open}");
    assert!(crate::tui::ui::worktree_pane_layout()
        .expect("files pane")
        .files_tab_active);

    assert!(super::commands::handle_files_command(&mut app, "/files off"));
    assert!(!app.diff_pane_focus, "closing Files returns focus to chat");
    render_and_snap(&app, &mut terminal);
    assert!(
        crate::tui::ui::worktree_pane_layout().is_none(),
        "clean explicit explorer should close completely"
    );
}

#[test]
fn test_files_preview_has_independent_keyboard_scroll() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let long_file = (1..=80)
        .map(|line| format!("pub const LINE_{line}: usize = {line};"))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(repo.path().join("demo.rs"), long_file).expect("write long preview");
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.open_project_files_pane();
    app.worktree_pane.tree_selected_path = Some("demo.rs".to_string());
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

    render_and_snap(&app, &mut terminal);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.worktree_pane.tree_preview_focused);
    for _ in 0..5 {
        assert!(app.handle_diff_pane_focus_key(KeyCode::Down, KeyModifiers::NONE));
    }
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("files pane");
    assert!(layout.preview_scroll > 0, "preview should scroll independently");
    assert_eq!(app.worktree_pane.tree_scroll, 0, "tree remains anchored");
}

#[test]
fn test_worktree_filter_expires_at_exactly_sixty_idle_seconds() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = worktree_filter_fixture();
    let last = app.worktree_pane.last_activity.unwrap();
    assert!(!app.update_worktree_file_filter(last + Duration::from_millis(59_999)));
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
    app.diff_pane_scroll = 17;
    app.diff_pane_scroll_x = 5;
    app.mouse_scroll_target = Some(MouseScrollTarget::SidePane);
    app.mouse_scroll_queue = 30;
    assert!(app.update_worktree_file_filter(last + Duration::from_secs(60)));
    assert_eq!(app.current_worktree_selected_file(), None);
    assert_eq!(
        (
            app.diff_pane_scroll,
            app.diff_pane_scroll_x,
            app.mouse_scroll_queue
        ),
        (0, 0, 0)
    );
    assert!(!app.update_worktree_file_filter(last + Duration::from_secs(61)));
    let text = render_and_snap(&app, &mut terminal);
    assert!(
        text.contains("fn new() {}") && text.contains("second"),
        "{text}"
    );
}

#[test]
fn test_worktree_filter_activity_is_scoped_to_its_pane() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, _terminal) = worktree_filter_fixture();
    let old = Instant::now() - Duration::from_secs(59);
    app.worktree_pane.last_activity = Some(old);
    app.handle_mouse_event(worktree_test_mouse(MouseEventKind::Moved, 2, 2));
    assert_eq!(
        app.worktree_pane.last_activity,
        Some(old),
        "chat must not extend timer"
    );
    let pane = crate::tui::ui::worktree_pane_layout().unwrap();
    for kind in [
        MouseEventKind::Moved,
        MouseEventKind::ScrollDown,
        MouseEventKind::Down(MouseButton::Right),
    ] {
        app.worktree_pane.last_activity = Some(old);
        app.handle_mouse_event(worktree_test_mouse(
            kind,
            pane.area.right() - 2,
            pane.area.bottom() - 2,
        ));
        let refreshed = app.worktree_pane.last_activity.unwrap();
        assert!(refreshed > old, "body event must refresh timer: {kind:?}");
        assert!(!app.update_worktree_file_filter(old + Duration::from_secs(60)));
    }
    app.worktree_pane.last_activity = Some(old);
    app.set_diff_pane_focus(true);
    app.handle_diff_pane_focus_key(KeyCode::End, KeyModifiers::NONE);
    assert!(
        app.worktree_pane.last_activity.unwrap() > old,
        "keyboard navigation refreshes timer"
    );
    app.worktree_pane.last_activity = Some(old);
    app.side_pane_scroll_by(1);
    assert!(
        app.worktree_pane.last_activity.unwrap() > old,
        "shared/native scrolling refreshes timer"
    );
}

#[test]
fn test_worktree_overflow_index_scrolls_independently_and_clicks_visible_file() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = worktree_filter_fixture();
    for n in 0..30 {
        std::fs::write(
            repo.path().join(format!("extra-{n:02}.txt")),
            format!("extra-content-{n:02}\n"),
        )
        .unwrap();
    }
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    assert!(layout.list_area.height < layout.paths.len() as u16);
    for _ in 0..20 {
        app.handle_mouse_event(worktree_test_mouse(
            MouseEventKind::ScrollDown,
            layout.list_area.x + 1,
            layout.list_area.y,
        ));
        render_and_snap(&app, &mut terminal);
    }
    let scrolled = crate::tui::ui::worktree_pane_layout().unwrap();
    assert_eq!(app.diff_pane_scroll, 0, "index wheels do not scroll body");
    assert_eq!(
        scrolled.list_scroll + scrolled.list_area.height as usize,
        scrolled.paths.len()
    );
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        scrolled.list_area.x + 1,
        scrolled.list_area.bottom() - 1,
    ));
    let text = render_and_snap(&app, &mut terminal);
    assert_eq!(app.current_worktree_selected_file(), Some("notes.txt"));
    assert!(
        text.contains("second") && !text.contains("fn new() {}"),
        "{text}"
    );
    assert_eq!(
        app.worktree_pane.list_scroll, scrolled.list_scroll,
        "selection preserves index viewport"
    );
}

#[test]
fn test_worktree_filter_removed_file_and_session_switch_return_to_all() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = worktree_filter_fixture();
    std::fs::write(repo.path().join("demo.rs"), "fn old() {}\n").unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    assert!(app.update_worktree_file_filter(Instant::now()));
    assert_eq!(app.current_worktree_selected_file(), None);
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("second"));
    let list = crate::tui::ui::worktree_pane_layout().unwrap().list_area;
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        list.x + 1,
        list.y,
    ));
    assert_eq!(app.current_worktree_selected_file(), Some("notes.txt"));
    app.session.id = "another-session".to_string();
    assert_eq!(
        app.current_worktree_selected_file(),
        None,
        "do not leak filter into another session in the same repo"
    );
    assert!(app.update_worktree_file_filter(Instant::now()));
}

#[test]
fn test_worktree_hidden_pane_clears_hit_targets_and_timeout_does_not_scroll_other_pane() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = worktree_filter_fixture();
    let list = crate::tui::ui::worktree_pane_layout().unwrap().list_area;
    app.side_panel = crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some("plan".into()),
        pages: vec![crate::side_panel::SidePanelPage {
            id: "plan".into(),
            title: "Plan".into(),
            file_path: "".into(),
            format: crate::side_panel::SidePanelPageFormat::Markdown,
            source: crate::side_panel::SidePanelPageSource::Managed,
            content: "plan\n".repeat(100),
            updated_at_ms: 1,
        }],
    };
    render_and_snap(&app, &mut terminal);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());
    let last = app.worktree_pane.last_activity.unwrap();
    assert!(!app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        list.x + 1,
        list.y
    )));
    assert_eq!(app.worktree_pane.last_activity, Some(last));
    app.diff_pane_scroll = 7;
    assert!(app.update_worktree_file_filter(last + Duration::from_secs(60)));
    assert_eq!(app.diff_pane_scroll, 7);
}

#[test]
fn test_worktree_file_list_stays_fixed_when_diff_scrolls() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::write(
        repo.path().join("notes.txt"),
        (0..100).map(|n| format!("note-{n}\n")).collect::<String>(),
    )
    .unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 24)).unwrap();
    render_and_snap(&app, &mut terminal);
    let pane = crate::tui::ui::last_layout_snapshot()
        .unwrap()
        .diff_pane_area
        .unwrap();
    let list_before = (1..3)
        .map(|dy| {
            (pane.x + 1..pane.right())
                .map(|x| terminal.backend().buffer()[(x, pane.y + dy)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    app.side_pane_scroll_by(50);
    let text = render_and_snap(&app, &mut terminal);
    let list_after = (1..3)
        .map(|dy| {
            (pane.x + 1..pane.right())
                .map(|x| terminal.backend().buffer()[(x, pane.y + dy)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        list_before, list_after,
        "file index must not scroll with diff"
    );
    assert!(text.contains("note-"));
    assert!(crate::tui::ui::last_diff_pane_effective_scroll() > 0);
}

#[test]
fn test_worktree_copy_body_coordinates_and_release_over_index() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, _terminal) = worktree_filter_fixture();
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    assert!(
        crate::tui::ui::copy_point_from_screen(layout.list_area.x, layout.list_area.y).is_none()
    );
    let point =
        crate::tui::ui::copy_point_from_screen(layout.body_area.x, layout.body_area.y).unwrap();
    assert_eq!(point.pane, crate::tui::CopySelectionPane::SidePane);
    let down = worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.body_area.x,
        layout.body_area.y + 2,
    );
    let drag = worktree_test_mouse(
        MouseEventKind::Drag(MouseButton::Left),
        layout.body_area.x + 7,
        layout.body_area.y + 3,
    );
    assert!(!app.handle_worktree_pane_mouse(down));
    app.handle_copy_selection_mouse_with(down, |_| panic!("down must not copy"));
    app.handle_copy_selection_mouse_with(drag, |_| panic!("drag must not copy"));
    assert!(app.copy_selection_dragging);
    let release = worktree_test_mouse(
        MouseEventKind::Up(MouseButton::Left),
        layout.list_area.x + 7,
        layout.list_area.y,
    );
    assert!(
        !app.handle_worktree_pane_mouse(release),
        "index must not swallow body drag release"
    );
    let mut copied = String::new();
    app.handle_copy_selection_mouse_with(release, |text| {
        copied = text.to_string();
        true
    });
    assert!(!app.copy_selection_dragging);
    assert!(!copied.is_empty(), "body selection must remain copyable");
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
}

#[test]
fn test_worktree_native_scrollbar_uses_only_body_geometry() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = worktree_filter_fixture();
    std::fs::write(repo.path().join("demo.rs"), "long-line\n".repeat(100)).unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    app.side_panel_native_scrollbar = true;
    app.diff_pane_scroll = usize::MAX;
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    let snapshot = app.native_scroll_snapshot_for_test();
    let pane = snapshot["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["y"].as_u64() == Some(layout.body_area.y as u64))
        .expect("body pane snapshot");
    assert_eq!(
        pane["height"].as_u64(),
        Some(layout.body_area.height as u64)
    );
    assert_eq!(
        pane["viewport_length"].as_u64(),
        Some(layout.body_area.height as u64)
    );
    assert_eq!(
        pane["position"].as_u64(),
        Some(crate::tui::ui::last_diff_pane_max_scroll() as u64)
    );
    assert_eq!(
        pane["content_length"].as_u64().unwrap() - pane["viewport_length"].as_u64().unwrap(),
        pane["position"].as_u64().unwrap()
    );
}

#[test]
fn test_worktree_resize_keeps_index_and_clears_hidden_hit_targets() {
    let _lock = scroll_render_test_lock();
    let (_repo, app, _terminal) = worktree_filter_fixture();
    for height in [8, 12, 24, 40] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, height)).unwrap();
        render_and_snap(&app, &mut terminal);
        if let Some(layout) = crate::tui::ui::worktree_pane_layout() {
            assert!(layout.list_area.height > 0);
            assert!(layout.body_area.y >= layout.list_area.bottom());
            assert!(layout.body_area.bottom() <= layout.area.bottom());
        }
    }
    let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    render_and_snap(&app, &mut narrow);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());
}

#[test]
fn test_worktree_filter_hidden_idle_expiry_restores_all_files_at_top() {
    let _lock = scroll_render_test_lock();
    for overlay in [false, true] {
        let (_repo, mut app, mut wide) = worktree_filter_fixture();
        app.diff_pane_scroll = 90;
        app.diff_pane_scroll_x = 12;
        if overlay {
            app.help_scroll = Some(0);
            render_and_snap(&app, &mut wide);
        } else {
            let mut narrow =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
            render_and_snap(&app, &mut narrow);
        }
        assert!(crate::tui::ui::worktree_pane_layout().is_none());
        let last = app.worktree_pane.last_activity.unwrap();
        assert!(app.update_worktree_file_filter(last + Duration::from_secs(60)));
        assert_eq!((app.diff_pane_scroll, app.diff_pane_scroll_x), (0, 0));
        app.help_scroll = None;
        let text = render_and_snap(&app, &mut wide);
        assert!(
            text.contains("fn new() {}") && text.contains("second"),
            "{text}"
        );
    }
}

#[test]
fn test_worktree_fallback_modes_reset_hidden_filter_scroll() {
    let _lock = scroll_render_test_lock();
    for mode in [
        crate::config::DiffDisplayMode::File,
        crate::config::DiffDisplayMode::Pinned,
    ] {
        let (_repo, mut app, mut wide) = worktree_filter_fixture();
        app.diff_mode = mode;
        render_and_snap(&app, &mut wide);
        assert!(crate::tui::ui::worktree_pane_layout().is_some());
        app.diff_pane_scroll = 90;
        let mut narrow =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        render_and_snap(&app, &mut narrow);
        assert!(crate::tui::ui::worktree_pane_layout().is_none());
        let last = app.worktree_pane.last_activity.unwrap();
        assert!(app.update_worktree_file_filter(last + Duration::from_secs(60)));
        assert_eq!(
            app.diff_pane_scroll, 0,
            "hidden worktree fallback in {mode:?}"
        );
    }
}

#[test]
fn test_worktree_file_click_filters_and_second_click_shows_all() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let all = render_and_snap(&app, &mut terminal);
    assert!(all.contains("fn new() {}") && all.contains("second"));
    let pane = crate::tui::ui::last_layout_snapshot()
        .unwrap()
        .diff_pane_area
        .unwrap();
    for (row, expected, excluded) in [
        (pane.y + 1, "fn new() {}", "second"),
        (pane.y + 2, "second", "fn new() {}"),
    ] {
        app.handle_mouse_event(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            pane.x + 3,
            row,
        ));
        app.handle_mouse_event(worktree_test_mouse(
            MouseEventKind::Up(MouseButton::Left),
            pane.x + 3,
            row,
        ));
        let selected = render_and_snap(&app, &mut terminal);
        assert!(selected.contains(expected), "{selected}");
        assert!(
            !selected.contains(excluded),
            "must only show selected file: {selected}"
        );
        assert!(
            selected.contains("demo.rs") && selected.contains("notes.txt"),
            "index remains complete"
        );
    }
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        pane.x + 3,
        pane.y + 2,
    ));
    let all_again = render_and_snap(&app, &mut terminal);
    assert!(
        all_again.contains("fn new() {}") && all_again.contains("second"),
        "{all_again}"
    );
}

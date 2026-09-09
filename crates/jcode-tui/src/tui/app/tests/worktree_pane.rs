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
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    render_and_snap(&app, &mut terminal);
    assert_eq!(
        app.current_worktree_selected_file(),
        Some("demo.rs"),
        "selected={:?} cwd={:?} cached={:?}",
        app.current_worktree_selected_file(),
        app.session.working_dir,
        crate::tui::ui::cached_worktree_paths(app.session.working_dir.as_deref())
    );
    (repo, app, terminal)
}

fn inspector_fixture() -> (
    tempfile::TempDir,
    App,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
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
    std::fs::write(repo.path().join("README.md"), "# Heading\n\n**bold**\n").unwrap();
    let script = std::iter::once("def old(): return 1".to_string())
        .chain((1..=79).map(|line| format!("def function_{line}(): return {line}")))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(repo.path().join("script.py"), script).unwrap();
    git(&["add", "README.md", "script.py"]);
    git(&["commit", "-qm", "inspector fixture"]);
    std::fs::write(repo.path().join("README.md"), "# Heading\n\n**changed**\n").unwrap();
    let script = std::iter::once("def new(): return 2".to_string())
        .chain((1..=79).map(|line| format!("def function_{line}(): return {}", line + 1)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(repo.path().join("script.py"), script).unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    crate::tui::ui::assert_cached_inspector_change_for_tests(repo.path(), "README.md");
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    let terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    (repo, app, terminal)
}

fn select_inspector_file(
    app: &mut App,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
    path: &str,
) {
    render_and_snap(app, terminal);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    let index = layout
        .tree_rows
        .iter()
        .position(|row| row.path == path)
        .unwrap();
    app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.tree_area.unwrap().x + 1,
        layout.tree_area.unwrap().y + index as u16,
    ));
    render_and_snap(app, terminal);
}

#[test]
fn files_inspector_markdown_read_source_and_changes_are_real_views() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "README.md");
    let read = render_and_snap(&app, &mut terminal);
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Read)
    );
    assert!(
        read.contains("Read") && read.contains("changed") && !read.contains("**changed**"),
        "read view: {read}"
    );
    assert!(
        app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .inspector_mode_areas[1]
                .0
                .x
                + 1,
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .inspector_mode_areas[1]
                .0
                .y
        ))
    );
    let source = render_and_snap(&app, &mut terminal);
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Source)
    );
    assert!(
        source.contains("**changed**"),
        "source view must preserve markdown syntax: {source}"
    );
    assert!(
        app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .inspector_mode_areas[2]
                .0
                .x
                + 1,
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .inspector_mode_areas[2]
                .0
                .y
        ))
    );
    let changes = render_and_snap(&app, &mut terminal);
    assert!(
        changes.contains("-**bold**") && changes.contains("+**changed**"),
        "changes view: {changes}"
    );
}

#[test]
fn files_inspector_python_exposes_only_source_and_changes_with_source_default() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    let text = render_and_snap(&app, &mut terminal);
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Source)
    );
    assert_eq!(
        app.files_inspector.available_modes().collect::<Vec<_>>(),
        vec![
            crate::tui::app::files_inspector::FileInspectorMode::Source,
            crate::tui::app::files_inspector::FileInspectorMode::Changes,
        ]
    );
    assert!(
        text.contains("def new()") && !text.contains(" Read "),
        "python source: {text}"
    );
}

#[test]
fn files_inspector_enter_focus_stays_in_files_without_side_panel_page() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.files_inspector.is_focused());
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    assert!(app.side_panel.pages.is_empty());
}

#[test]
fn files_inspector_left_then_escape_returns_to_chat() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE);
    app.handle_diff_pane_focus_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(!app.files_inspector.is_focused());
    assert!(app.diff_pane_focus);
    app.handle_diff_pane_focus_key(KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.diff_pane_focus);
}

#[test]
fn files_inspector_scroll_is_per_file_and_mode() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE);
    for _ in 0..3 {
        app.handle_diff_pane_focus_key(KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(app.files_inspector.scroll_offset() > 0);
    app.handle_diff_pane_focus_key(KeyCode::Left, KeyModifiers::NONE);
    select_inspector_file(&mut app, &mut terminal, "README.md");
    assert_eq!(app.files_inspector.scroll_offset(), 0);
}

#[test]
fn files_inspector_right_cycles_only_supported_modes_and_preserves_offsets() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Source)
    );
    app.files_inspector.set_scroll_offset(7);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Changes)
    );
    app.files_inspector.set_scroll_offset(11);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Left, KeyModifiers::NONE));
    assert!(!app.files_inspector.is_focused());
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Changes)
    );
    assert_eq!(app.files_inspector.scroll_offset(), 11);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Left, KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Changes)
    );
}

#[test]
fn files_inspector_home_end_are_per_mode_and_tab_leaves_focus() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    app.files_inspector.set_scroll_offset(5);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    app.files_inspector.set_scroll_offset(9);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(app.files_inspector.scroll_offset(), 0);
    assert!(app.handle_diff_pane_focus_key(KeyCode::End, KeyModifiers::NONE));
    assert!(app.files_inspector.scroll_offset() > 0);
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    assert!(!app.files_inspector.is_focused());
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);
    assert!(!app.files_inspector.is_focused());
}

#[test]
fn files_inspector_unfocused_preview_wheel_isolated_to_selected_offset() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    let layout = crate::tui::ui::worktree_pane_layout().expect("files pane layout");
    let selected = app.worktree_pane.tree_selected_path.clone();
    let tree_scroll = app.worktree_pane.tree_scroll;
    assert!(!app.files_inspector.is_focused());
    app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::ScrollDown,
        layout.preview_area.expect("preview area").x + 1,
        layout.preview_area.expect("preview area").y + 1,
    ));
    assert!(!app.files_inspector.is_focused());
    assert!(app.files_inspector.scroll_offset() > 0);
    assert_eq!(app.worktree_pane.tree_selected_path, selected);
    assert_eq!(app.worktree_pane.tree_scroll, tree_scroll);
    assert!(
        app.diff_pane_focus,
        "wheel must not change chat focus state"
    );
}

#[test]
fn files_inspector_directory_selection_clears_identity_and_focus() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = inspector_fixture();
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/keep.txt"), "keep\n").unwrap();
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.files_inspector.enter_focus());
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    let index = layout
        .tree_rows
        .iter()
        .position(|row| row.path == "src")
        .unwrap();
    app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.tree_area.unwrap().x + 1,
        layout.tree_area.unwrap().y + index as u16,
    ));
    assert!(app.files_inspector.selected_file().is_none());
    assert!(!app.files_inspector.is_focused());
}

#[test]
fn files_inspector_session_and_root_changes_clear_identity() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.files_inspector.enter_focus());
    app.session.id = "changed-session".into();
    app.prepare_worktree_pane_state();
    assert!(app.files_inspector.selected_file().is_none());
    select_inspector_file(&mut app, &mut terminal, "script.py");
    app.session.working_dir = Some("/different-root".into());
    app.prepare_worktree_pane_state();
    assert!(app.files_inspector.selected_file().is_none());
}

#[test]
fn files_inspector_focus_exits_when_resize_removes_preview() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    assert!(app.files_inspector.enter_focus());
    let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 24)).unwrap();
    render_and_snap(&app, &mut narrow);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());
    app.handle_project_files_focus_key(KeyCode::Down);
    assert!(!app.files_inspector.is_focused());
}

#[test]
fn connected_and_disconnected_enter_files_inspector() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = inspector_fixture();
    select_inspector_file(&mut app, &mut terminal, "script.py");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();
    rt.block_on(app.handle_remote_key(KeyCode::Enter, KeyModifiers::NONE, &mut remote))
        .unwrap();
    assert!(app.files_inspector.is_focused());
    app.files_inspector.exit_focus();
    super::remote::handle_disconnected_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    assert!(app.files_inspector.is_focused());
}

#[test]
fn test_worktree_header_has_padded_heavy_boundary() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
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

    assert!(
        layout.files_tab_active,
        "Files tab should be active: {text}"
    );
    assert!(
        text.contains("Diff") && text.contains("Files"),
        "tab header: {text}"
    );
    assert!(text.contains("demo.rs"), "project tree: {text}");
    assert!(
        text.contains("fn new() {}"),
        "selected file preview: {text}"
    );
    assert!(layout.tree_area.is_some());
    assert!(layout.preview_area.is_some());
}

#[test]
fn test_files_pane_is_visible_and_active_by_default() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("default files pane");
    assert!(
        layout.files_tab_active,
        "Files should be the default tab: {text}"
    );
    assert!(text.contains("demo.rs"), "default project tree: {text}");
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
    assert!(
        !collapsed.contains("lib.rs"),
        "children start hidden: {collapsed}"
    );

    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    let expanded = render_and_snap(&app, &mut terminal);
    assert!(expanded.contains("▾ src"), "expanded tree: {expanded}");
    assert!(expanded.contains("lib.rs"), "expanded child: {expanded}");

    assert!(app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE));
    let diff = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("diff pane layout");
    assert!(
        !layout.files_tab_active,
        "Tab should switch to Diff: {diff}"
    );
    assert!(
        diff.contains("changes"),
        "dirty diff should render after switch: {diff}"
    );
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
    assert!(
        !crate::tui::ui::worktree_pane_layout()
            .expect("diff pane")
            .files_tab_active
    );
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
    assert!(
        crate::tui::ui::worktree_pane_layout()
            .expect("files pane")
            .files_tab_active
    );

    assert!(super::commands::handle_files_command(
        &mut app,
        "/files off"
    ));
    assert!(!app.diff_pane_focus, "closing Files returns focus to chat");
    render_and_snap(&app, &mut terminal);
    assert!(
        crate::tui::ui::worktree_pane_layout().is_none(),
        "clean explicit explorer should close completely"
    );
}

#[test]
fn test_files_visibility_and_tab_choice_survive_session_transition() {
    let _lock = scroll_render_test_lock();
    let clean = tempfile::tempdir().expect("clean project");
    std::process::Command::new("git")
        .current_dir(clean.path())
        .args(["init", "-q"])
        .status()
        .expect("git init");
    crate::tui::ui::prime_project_tree_for_tests(clean.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(clean.path().to_string_lossy().into_owned());
    assert!(super::commands::handle_files_command(
        &mut app,
        "/files off"
    ));

    app.session.id = "next-clean-session".to_string();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 24)).unwrap();
    render_and_snap(&app, &mut terminal);
    assert!(
        crate::tui::ui::worktree_pane_layout().is_none(),
        "explicit off should survive a session transition"
    );

    let dirty = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(dirty.path());
    crate::tui::ui::prime_project_tree_for_tests(dirty.path());
    app.session.working_dir = Some(dirty.path().to_string_lossy().into_owned());
    app.session.id = "dirty-session".to_string();
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.session.id = "next-dirty-session".to_string();
    render_and_snap(&app, &mut terminal);
    assert!(
        !crate::tui::ui::worktree_pane_layout()
            .expect("dirty right pane")
            .files_tab_active,
        "Diff choice should survive a session transition"
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
    assert!(
        layout.preview_scroll > 0,
        "preview should scroll independently"
    );
    assert_eq!(app.worktree_pane.tree_scroll, 0, "tree remains anchored");
}

#[test]
fn test_files_tree_opens_markdown_documents_case_insensitively_and_deduplicates() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::write(repo.path().join("README.md"), "# Read me\n").expect("write Markdown");
    std::fs::write(repo.path().join("NOTES.MD"), "# Notes\n").expect("write uppercase Markdown");
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();

    select_inspector_file(&mut app, &mut terminal, "README.md");
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("files pane layout");
    assert!(
        layout.preview_area.is_some(),
        "Markdown gets exactly one inline inspector"
    );
    assert!(text.contains("Read") && text.contains("Files"));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    assert!(app.files_inspector.is_focused());
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Read)
    );
}

#[test]
fn test_documents_body_wheel_uses_smooth_side_pane_scroll_without_changing_focus() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot(
        "one",
        &[(
            "one",
            "One",
            &(1..=120)
                .map(|line| format!("document line {line}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )],
    ));
    app.set_diff_pane_focus(false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("documents pane layout");

    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::ScrollDown,
        layout.body_area.x + 1,
        layout.body_area.y + 1,
    ));

    assert!(
        app.diff_pane_scroll > 0,
        "wheel over document body should advance the rendered side pane"
    );
    assert!(!app.diff_pane_focus, "wheel must not change keyboard focus");
}

#[test]
fn test_empty_composer_horizontal_arrows_navigate_worktree_tabs_and_preserve_text_cursor() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.set_diff_pane_focus(false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);

    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(app.diff_pane_focus);
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert!(!app.diff_pane_focus);

    app.handle_key(KeyCode::Right, KeyModifiers::SHIFT).unwrap();
    assert!(
        !app.diff_pane_focus,
        "modified arrows must not traverse into a worktree pane"
    );

    app.set_input_for_test("ab");
    app.cursor_pos = 1;
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(app.cursor_pos, 2, "nonempty composer keeps cursor movement");
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(app.cursor_pos, 1, "nonempty composer keeps cursor movement");
}

#[test]
fn test_remote_empty_composer_horizontal_arrows_match_local_navigation() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.set_diff_pane_focus(false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut remote = crate::tui::backend::RemoteConnection::dummy();

    rt.block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .unwrap();
    assert!(app.diff_pane_focus);
    rt.block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    rt.block_on(app.handle_remote_key(KeyCode::Right, KeyModifiers::NONE, &mut remote))
        .unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );

    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.set_diff_pane_focus(false);
    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(app.diff_pane_focus);
    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );
}

#[test]
fn test_disconnected_files_navigation_opens_markdown_after_arrow_traversal() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::write(repo.path().join("README.md"), "# Read me\n").expect("write Markdown");
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    app.worktree_pane.tree_selected_path = Some("demo.rs".into());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);

    super::remote::handle_disconnected_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    super::remote::handle_disconnected_key(&mut app, KeyCode::Down, KeyModifiers::NONE).unwrap();
    assert_eq!(
        app.worktree_pane.tree_selected_path.as_deref(),
        Some("README.md")
    );
    super::remote::handle_disconnected_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();

    assert!(
        app.side_panel
            .focused_page()
            .is_some_and(|page| page.file_path.ends_with("README.md")),
        "disconnected activation should open the selected Markdown page"
    );
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );
}

#[test]
fn test_disconnected_diagram_focus_consumes_right_before_worktree_traversal() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.diagram_mode = crate::config::DiagramDisplayMode::Pinned;
    app.diagram_pane_enabled = true;
    app.diagram_focus = true;
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    crate::tui::mermaid::clear_active_diagrams();
    crate::tui::mermaid::register_active_diagram(0x1, 100, 80, None);

    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();

    assert_eq!(app.diagram_scroll_x, 4);
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    crate::tui::mermaid::clear_active_diagrams();
}

#[test]
fn test_empty_composer_navigation_ignores_hidden_or_generic_worktree_surfaces() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    let mut narrow = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 24)).unwrap();
    render_and_snap(&app, &mut narrow);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());

    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(
        !app.diff_pane_focus,
        "Right must not focus a hidden worktree pane"
    );

    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.set_diff_pane_focus(true);
    // A retained generic side panel must not make an old worktree layout focusable.
    render_and_snap(&app, &mut narrow);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(
        !app.diff_pane_focus,
        "stale right-pane focus returns to Chat"
    );

    app.handle_key(KeyCode::Right, KeyModifiers::SHIFT).unwrap();
    assert!(
        !app.diff_pane_focus,
        "modified arrows must not traverse hidden panes"
    );
}

#[test]
fn test_hiding_active_documents_normalizes_worktree_tab_and_availability() {
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    assert!(app.worktree_documents_available());
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );

    app.toggle_side_panel();

    assert!(!app.worktree_documents_available());
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
}

#[test]
fn test_files_tree_non_markdown_activation_keeps_inline_preview_focus() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    app.worktree_pane.tree_selected_path = Some("demo.rs".into());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();

    render_and_snap(&app, &mut terminal);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.worktree_pane.tree_preview_focused);
    assert!(app.side_panel.pages.is_empty());
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
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
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
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

fn document_snapshot(
    focused: &str,
    pages: &[(&str, &str, &str)],
) -> crate::side_panel::SidePanelSnapshot {
    crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some(focused.into()),
        pages: pages
            .iter()
            .map(|(id, title, content)| crate::side_panel::SidePanelPage {
                id: (*id).into(),
                title: (*title).into(),
                file_path: String::new(),
                format: crate::side_panel::SidePanelPageFormat::Markdown,
                source: crate::side_panel::SidePanelPageSource::Managed,
                content: (*content).into(),
                updated_at_ms: 1,
            })
            .collect(),
    }
}

fn linked_document_snapshot(
    focused: &str,
    pages: &[(&str, &str, &std::path::Path, &str)],
) -> crate::side_panel::SidePanelSnapshot {
    crate::side_panel::SidePanelSnapshot {
        focused_page_id: Some(focused.into()),
        pages: pages
            .iter()
            .map(
                |(id, title, file_path, content)| crate::side_panel::SidePanelPage {
                    id: (*id).into(),
                    title: (*title).into(),
                    file_path: file_path.to_string_lossy().into_owned(),
                    format: crate::side_panel::SidePanelPageFormat::Markdown,
                    source: crate::side_panel::SidePanelPageSource::LinkedFile,
                    content: (*content).into(),
                    updated_at_ms: 1,
                },
            )
            .collect(),
    }
}

#[test]
fn test_documents_join_worktree_tabs_and_cycle_without_duplicate_pages() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.diff_mode = crate::config::DiffDisplayMode::Inline;
    app.apply_side_panel_snapshot(document_snapshot(
        "one",
        &[("one", "One", "# one"), ("two", "Two", "# two")],
    ));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    app.set_diff_pane_focus(true);
    let layout = crate::tui::ui::worktree_pane_layout().expect("documents pane layout");
    assert!(text.contains("Diff") && text.contains("Files") && text.contains("Documents"));
    assert!(text.contains("one"));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    assert!(app.handle_diff_pane_focus_key(KeyCode::BackTab, KeyModifiers::NONE));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char(']'), KeyModifiers::NONE));
    assert_eq!(app.side_panel.focused_page_id.as_deref(), Some("two"));
    assert_eq!(app.side_panel.pages.len(), 2);
    assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.files_tab_area.x + 1,
        layout.files_tab_area.y,
    )));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
}

#[test]
fn test_documents_mouse_header_controls_select_modes_and_pages() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(document_snapshot(
        "one",
        &[("one", "One", "# one"), ("two", "Two", "# two")],
    ));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(200, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("documents layout");

    for (area, expected) in [
        (
            layout.document_source_area,
            super::worktree_pane::MarkdownDocumentMode::Source,
        ),
        (
            layout.document_changes_area,
            super::worktree_pane::MarkdownDocumentMode::Changes,
        ),
        (
            layout.document_read_area,
            super::worktree_pane::MarkdownDocumentMode::Read,
        ),
    ] {
        assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 1,
            area.y,
        )));
        assert_eq!(app.focused_markdown_document_mode(), expected);
    }
    assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.document_next_page_area.x + 1,
        layout.document_next_page_area.y,
    )));
    assert_eq!(app.side_panel.focused_page_id.as_deref(), Some("two"));
    assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        layout.document_previous_page_area.x + 1,
        layout.document_previous_page_area.y,
    )));
    assert_eq!(app.side_panel.focused_page_id.as_deref(), Some("one"));
}

#[test]
fn test_document_header_hit_rectangles_match_rendered_cells_only() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("documents layout");

    for area in [
        layout.diff_tab_area,
        layout.files_tab_area,
        layout.documents_tab_area,
    ] {
        assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        )));
        assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.right() - 1,
            area.y,
        )));
    }

    for (mode, before) in [
        (
            super::worktree_pane::MarkdownDocumentMode::Read,
            super::worktree_pane::MarkdownDocumentMode::Source,
        ),
        (
            super::worktree_pane::MarkdownDocumentMode::Source,
            super::worktree_pane::MarkdownDocumentMode::Changes,
        ),
        (
            super::worktree_pane::MarkdownDocumentMode::Changes,
            super::worktree_pane::MarkdownDocumentMode::Read,
        ),
    ] {
        app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Documents);
        while app.focused_markdown_document_mode() != before {
            assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
        }
        render_and_snap(&app, &mut terminal);
        let area = match mode {
            super::worktree_pane::MarkdownDocumentMode::Read => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_read_area
            }
            super::worktree_pane::MarkdownDocumentMode::Source => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_source_area
            }
            super::worktree_pane::MarkdownDocumentMode::Changes => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_changes_area
            }
        };
        assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        )));
        render_and_snap(&app, &mut terminal);
        while app.focused_markdown_document_mode() != before {
            assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
        }
        render_and_snap(&app, &mut terminal);
        let area = match mode {
            super::worktree_pane::MarkdownDocumentMode::Read => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_read_area
            }
            super::worktree_pane::MarkdownDocumentMode::Source => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_source_area
            }
            super::worktree_pane::MarkdownDocumentMode::Changes => {
                crate::tui::ui::worktree_pane_layout()
                    .unwrap()
                    .document_changes_area
            }
        };
        assert!(app.handle_worktree_pane_mouse(worktree_test_mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.right() - 1,
            area.y,
        )));
    }

    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Documents);
    render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("documents layout");
    for x in layout.area.x..layout.area.x + 3 {
        assert!(
            !app.handle_worktree_pane_mouse(worktree_test_mouse(
                MouseEventKind::Down(MouseButton::Left),
                x,
                layout.area.y,
            )),
            "left chrome/padding cell {x} must not be clickable"
        );
        assert_eq!(
            app.worktree_pane.tab,
            super::worktree_pane::WorktreePaneTab::Documents
        );
    }
}

#[test]
fn test_documents_keep_tabs_and_controls_on_narrow_terminal() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 24)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    let layout = crate::tui::ui::worktree_pane_layout().expect("narrow documents layout");
    assert!(text.contains("Diff") && text.contains("Files") && text.contains("Documents"));
    app.set_diff_pane_focus(true);
    for expected in [
        super::worktree_pane::MarkdownDocumentMode::Source,
        super::worktree_pane::MarkdownDocumentMode::Changes,
        super::worktree_pane::MarkdownDocumentMode::Read,
    ] {
        assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
        assert_eq!(app.focused_markdown_document_mode(), expected);
    }
    assert!(layout.document_read_area.width > 0);
}

#[test]
fn test_invalid_focused_document_snapshot_normalizes_to_an_existing_read_page() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("missing", &[("one", "One", "# one")]));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    let text = render_and_snap(&app, &mut terminal);

    assert_eq!(app.side_panel.focused_page_id.as_deref(), Some("one"));
    assert_eq!(
        app.focused_markdown_document_mode(),
        super::worktree_pane::MarkdownDocumentMode::Read
    );
    assert!(
        crate::tui::ui::worktree_pane_layout().is_some(),
        "Documents must remain visible: {text}"
    );
}

#[test]
fn test_invalid_focused_document_snapshot_resets_existing_page_to_read_mode() {
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    assert!(app.cycle_markdown_document_mode());
    assert_eq!(
        app.focused_markdown_document_mode(),
        super::worktree_pane::MarkdownDocumentMode::Source
    );

    app.apply_side_panel_snapshot(document_snapshot("missing", &[("one", "One", "# one")]));

    assert_eq!(app.side_panel.focused_page_id.as_deref(), Some("one"));
    assert_eq!(
        app.focused_markdown_document_mode(),
        super::worktree_pane::MarkdownDocumentMode::Read,
        "fallback must not retain the old page mode"
    );
}

#[test]
fn test_narrow_clean_session_does_not_auto_open_files_without_documents() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(70, 24)).unwrap();

    render_and_snap(&app, &mut terminal);
    assert!(crate::tui::ui::worktree_pane_layout().is_none());
}

#[test]
fn test_markdown_reopen_from_files_stays_in_files_inspector() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::write(repo.path().join("README.md"), "# Read me\n").expect("write Markdown");
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    select_inspector_file(&mut app, &mut terminal, "README.md");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    assert!(app.files_inspector.is_focused());
    assert_eq!(app.side_panel.pages.len(), 0);
    assert_eq!(
        app.files_inspector.mode(),
        Some(crate::tui::app::files_inspector::FileInspectorMode::Read)
    );
}

#[test]
fn test_session_transition_without_documents_clears_documents_tab_state() {
    let mut app = create_test_app();
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", "# one")]));
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Documents
    );

    app.session.id = "session-without-documents".into();
    app.apply_side_panel_snapshot(crate::side_panel::SidePanelSnapshot::default());

    assert!(app.worktree_pane.document_ui.is_empty());
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
}

#[test]
fn test_document_mode_and_scroll_are_per_page_and_survive_passive_refresh() {
    let mut app = create_test_app();
    let first = document_snapshot("one", &[("one", "One", "one\n"), ("two", "Two", "two\n")]);
    app.apply_side_panel_snapshot(first.clone());
    app.set_diff_pane_focus(true);
    app.diff_pane_scroll = 12;
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(
        app.markdown_document_mode("one"),
        super::worktree_pane::MarkdownDocumentMode::Source
    );
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char(']'), KeyModifiers::NONE));
    app.diff_pane_scroll = 34;
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(
        app.markdown_document_mode("two"),
        super::worktree_pane::MarkdownDocumentMode::Source
    );
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('['), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 0);
    assert_eq!(
        app.markdown_document_mode("one"),
        super::worktree_pane::MarkdownDocumentMode::Source
    );

    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);
    app.apply_side_panel_snapshot(first);
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Files
    );
    assert_eq!(app.diff_pane_scroll, 0);
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Documents);
    assert_eq!(app.diff_pane_scroll, 0);
    assert_eq!(
        app.markdown_document_mode("one"),
        super::worktree_pane::MarkdownDocumentMode::Source
    );
}

#[test]
fn test_document_scroll_is_independent_for_each_mode() {
    let mut app = create_test_app();
    let content = "one\n".repeat(80);
    app.apply_side_panel_snapshot(document_snapshot("one", &[("one", "One", &content)]));
    app.set_diff_pane_focus(true);

    app.diff_pane_scroll = 12;
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 0, "unseen Source starts at the top");

    app.diff_pane_scroll = 34;
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 0, "unseen Changes starts at the top");

    app.diff_pane_scroll = 56;
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 12, "Read scroll is restored");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 34, "Source scroll is restored");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert_eq!(app.diff_pane_scroll, 56, "Changes scroll is restored");
}

#[test]
fn test_document_scroll_survives_refresh_but_not_a_new_session() {
    let mut app = create_test_app();
    let snapshot = document_snapshot("one", &[("one", "One", "one")]);
    app.apply_side_panel_snapshot(snapshot.clone());
    app.diff_pane_scroll = 23;
    app.save_focused_document_ui();
    app.apply_side_panel_snapshot(snapshot.clone());
    assert_eq!(app.diff_pane_scroll, 23, "passive refresh preserves scroll");

    app.session.id = "new-session".into();
    app.apply_side_panel_snapshot(snapshot);
    assert_eq!(app.diff_pane_scroll, 0, "new sessions start at the top");
    assert_eq!(
        app.focused_markdown_document_mode(),
        super::worktree_pane::MarkdownDocumentMode::Read,
        "new sessions initialize the focused document state"
    );
}

#[test]
fn test_session_transition_with_same_focused_document_id_initializes_documents_workspace() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    let snapshot = document_snapshot("one", &[("one", "One", "# one")]);
    app.apply_side_panel_snapshot(snapshot.clone());

    app.session.id = "new-session".into();
    app.apply_side_panel_snapshot(snapshot);

    assert!(
        app.worktree_pane.document_ui.contains_key("one"),
        "new session must initialize its colliding focused page"
    );
    assert_eq!(
        app.focused_markdown_document_mode(),
        super::worktree_pane::MarkdownDocumentMode::Read
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(
        crate::tui::ui::worktree_pane_layout().is_some(),
        "Documents workspace must remain visible: {text}"
    );
}

#[test]
fn test_documents_render_mode_header_and_raw_source() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(document_snapshot(
        "one",
        &[(
            "one",
            "One",
            "# heading\n\n**bold**\n\n```rust\nlet x = 1;\n```\n",
        )],
    ));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let read = render_and_snap(&app, &mut terminal);
    assert!(
        read.contains("Read") && read.contains("Source") && read.contains("Changes"),
        "{read}"
    );
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    let source = render_and_snap(&app, &mut terminal);
    assert!(source.contains("# heading"), "{source}");
    assert!(source.contains("**bold**"), "{source}");
    assert!(source.contains("```rust"), "{source}");
}

#[test]
fn test_documents_changes_shows_only_the_focused_linked_file_diff() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(linked_document_snapshot(
        "demo",
        &[
            (
                "demo",
                "Demo",
                &repo.path().join("demo.rs"),
                "document body",
            ),
            (
                "notes",
                "Notes",
                &repo.path().join("notes.txt"),
                "other document",
            ),
        ],
    ));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("Changes"), "{text}");
    assert!(
        text.contains("fn old() {}") && text.contains("fn new() {}"),
        "{text}"
    );
    assert!(
        !text.contains("second"),
        "must not leak another file: {text}"
    );
    assert!(
        !text.contains("document body"),
        "must not render Read content: {text}"
    );
}

#[test]
fn test_documents_changes_shows_safe_message_without_matching_linked_change() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(linked_document_snapshot(
        "missing",
        &[(
            "missing",
            "Missing",
            &repo.path().join("unchanged.md"),
            "document body",
        )],
    ));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("No changes for this document"), "{text}");
    assert!(!text.contains("document body"), "{text}");
}

#[test]
fn test_documents_changes_shows_safe_message_for_managed_page() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(document_snapshot(
        "managed",
        &[("managed", "Managed", "document body")],
    ));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("No changes for this document"), "{text}");
    assert!(!text.contains("document body"), "{text}");
}

#[test]
fn test_documents_changes_shows_safe_message_while_snapshot_is_unavailable() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(linked_document_snapshot(
        "demo",
        &[(
            "demo",
            "Demo",
            &repo.path().join("demo.rs"),
            "document body",
        )],
    ));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("Loading changes…"), "{text}");
    assert!(
        text.contains("Documents") && text.contains("Changes"),
        "{text}"
    );
    assert!(!text.contains("No changes for this document"), "{text}");
    assert!(!text.contains("document body"), "{text}");
}

#[test]
fn test_documents_changes_shows_deleted_focused_linked_file_diff() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    std::fs::remove_file(repo.path().join("demo.rs")).expect("delete tracked linked file");
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.apply_side_panel_snapshot(linked_document_snapshot(
        "demo",
        &[(
            "demo",
            "Demo",
            &repo.path().join("demo.rs"),
            "last good linked content",
        )],
    ));
    app.set_diff_pane_focus(true);
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));
    assert!(app.handle_diff_pane_focus_key(KeyCode::Char('m'), KeyModifiers::NONE));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("fn old() {}"), "{text}");
    assert!(!text.contains("No changes for this document"), "{text}");
    assert!(!text.contains("last good linked content"), "{text}");
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
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
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

#[test]
fn diff_routed_list_content_and_reentry_transitions_preserve_selection() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = worktree_filter_fixture();
    let list = crate::tui::ui::worktree_pane_layout().unwrap().list_area;
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Down(MouseButton::Left),
        list.x + 1,
        list.y,
    ));
    let _ = render_and_snap(&app, &mut terminal);
    app.set_diff_pane_focus(false);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert_eq!(app.current_worktree_selected_file(), Some("notes.txt"));
    app.handle_key(KeyCode::Up, KeyModifiers::NONE);
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE);
    let before = app.diff_pane_scroll;
    app.handle_key(KeyCode::Down, KeyModifiers::NONE);
    assert!(app.diff_pane_scroll >= before);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE);
    assert_eq!(
        app.worktree_pane.tab,
        super::worktree_pane::WorktreePaneTab::Diff
    );
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
    app.handle_key(KeyCode::Esc, KeyModifiers::NONE);
    app.handle_key(KeyCode::Left, KeyModifiers::NONE);
    assert!(!app.diff_pane_focus);
    let _ = render_and_snap(&app, &mut terminal);
}

#[test]
fn disconnected_diff_content_arrows_do_not_escape() {
    let _lock = scroll_render_test_lock();
    let (_repo, mut app, mut terminal) = worktree_filter_fixture();
    app.set_diff_pane_focus(false);
    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    super::remote::handle_disconnected_key(&mut app, KeyCode::Enter, KeyModifiers::NONE).unwrap();
    let tab = app.worktree_pane.tab;
    super::remote::handle_disconnected_key(&mut app, KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, tab);
    super::remote::handle_disconnected_key(&mut app, KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, tab);
    let _ = render_and_snap(&app, &mut terminal);
}

#[test]
fn cached_diff_lookup_uses_git_root_cache_for_subdirectory_and_empty_miss() {
    let _lock = scroll_render_test_lock();
    let (repo, _app, _terminal) = worktree_filter_fixture();
    let subdir = repo.path().join("nested");
    std::fs::create_dir_all(&subdir).unwrap();
    let subdir_text = subdir.to_string_lossy();
    let paths = crate::tui::ui::cached_worktree_paths(Some(&subdir_text));
    assert!(
        paths.iter().any(|path| path == "demo.rs"),
        "paths={paths:?}"
    );
    assert_eq!(
        crate::tui::ui::worktree_file_is_present(Some(&subdir_text), "demo.rs"),
        Some(true)
    );
    let missing = tempfile::tempdir().unwrap();
    let missing_text = missing.path().to_string_lossy();
    assert!(crate::tui::ui::cached_worktree_paths(Some(&missing_text)).is_empty());
    assert_eq!(
        crate::tui::ui::worktree_file_is_present(Some(&missing_text), "demo.rs"),
        None
    );
}

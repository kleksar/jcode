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
    // Entering the list selects a path but is not a timed content interaction.
    // Exercise a real pane event before testing idle expiry.
    let pane = crate::tui::ui::worktree_pane_layout().unwrap();
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::Moved,
        pane.body_area.x + 1,
        pane.body_area.y,
    ));
    assert!(app.worktree_pane.last_activity.is_some());
    (repo, app, terminal)
}

fn diff_route_fixture() -> (
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
    app.set_diff_pane_focus(false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
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

fn worktree_rendered_region(
    terminal: &ratatui::Terminal<ratatui::backend::TestBackend>,
    area: ratatui::layout::Rect,
) -> String {
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn files_inspector_enter_focus_stays_in_files_without_side_panel_page() {
    use crate::tui::app::files_inspector::FileInspectorMode::{Changes, Read, Source};
    let _lock = scroll_render_test_lock();
    for (path, modes, body) in [
        ("README.md", vec![Read, Source, Changes], "changed"),
        ("script.py", vec![Source, Changes], "def new(): return 2"),
    ] {
        let (_repo, mut app, mut terminal) = inspector_fixture();
        select_inspector_file(&mut app, &mut terminal, path);
        let before = crate::tui::ui::worktree_pane_layout().unwrap();
        let tree = before.tree_area.unwrap();
        let preview = before.preview_area.unwrap();
        let tree_text = worktree_rendered_region(&terminal, tree);
        let preview_text = worktree_rendered_region(&terminal, preview);
        assert!(tree.height > 0 && tree.bottom() <= preview.y);
        assert!(preview.bottom() <= before.area.bottom());
        assert!(tree_text.contains("README.md") && tree_text.contains("script.py"));
        assert!(preview_text.contains(body), "{preview_text}");
        let controls = ratatui::layout::Rect::new(
            preview.x,
            tree.bottom(),
            preview.width,
            preview.y - tree.bottom(),
        );
        let controls_text = worktree_rendered_region(&terminal, controls);
        assert!(controls_text.contains(path), "{controls_text}");
        assert!(controls_text.contains("Enter to scroll"));
        let mode_text = before
            .inspector_mode_areas
            .iter()
            .map(|(area, _)| worktree_rendered_region(&terminal, *area))
            .collect::<Vec<_>>();
        assert_eq!(
            before
                .inspector_mode_areas
                .iter()
                .map(|(_, mode)| *mode)
                .collect::<Vec<_>>(),
            modes
        );
        for (area, mode) in &before.inspector_mode_areas {
            assert!(area.y >= tree.bottom() && area.bottom() <= preview.y);
            assert!(area.x >= preview.x && area.right() <= preview.right());
            assert!(
                worktree_rendered_region(&terminal, *area).contains(match mode {
                    Read => "Read",
                    Source => "Source",
                    Changes => "Changes",
                })
            );
        }
        app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
        assert!(app.files_inspector.is_focused());
        render_and_snap(&app, &mut terminal);
        let after = crate::tui::ui::worktree_pane_layout().unwrap();
        assert_eq!(after.area, before.area);
        assert_eq!(after.tree_area, Some(tree));
        assert_eq!(after.preview_area, Some(preview));
        assert_eq!(after.inspector_mode_areas, before.inspector_mode_areas);
        let focused_controls = worktree_rendered_region(&terminal, controls);
        assert!(focused_controls.contains(path));
        assert!(focused_controls.contains("preview focus"));
        assert_eq!(
            after
                .inspector_mode_areas
                .iter()
                .map(|(area, _)| worktree_rendered_region(&terminal, *area))
                .collect::<Vec<_>>(),
            mode_text
        );
        assert_eq!(worktree_rendered_region(&terminal, tree), tree_text);
        assert_eq!(worktree_rendered_region(&terminal, preview), preview_text);
        assert_eq!(app.worktree_pane.tree_selected_path.as_deref(), Some(path));
        assert_eq!(
            app.worktree_pane.tab,
            super::worktree_pane::WorktreePaneTab::Files
        );
        assert!(app.side_panel.pages.is_empty());
    }
}

#[test]
fn routed_connected_disconnected_worktree_titles_lose_focus_without_losing_content() {
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    use ratatui::style::Modifier;
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        for tab in [Diff, Files] {
            let (_repo, mut app, mut terminal) = inspector_fixture();
            select_inspector_file(&mut app, &mut terminal, "script.py");
            app.files_inspector.set_scroll_offset(3);
            app.set_worktree_pane_tab(tab);
            app.set_diff_pane_focus(true);
            if tab == Diff {
                app.worktree_pane.selected_file = Some("script.py".into());
                app.diff_pane_scroll = 2;
                app.diff_pane_auto_scroll = false;
            }
            render_and_snap(&app, &mut terminal);
            let before = crate::tui::ui::worktree_pane_layout().unwrap();
            let title = if tab == Diff {
                before.diff_tab_area
            } else {
                before.files_tab_area
            };
            let content_area = if tab == Diff {
                before.body_area
            } else {
                before.preview_area.unwrap()
            };
            let content = worktree_rendered_region(&terminal, content_area);
            assert!(content.contains("function_"), "{content}");
            let cell = &terminal.backend().buffer()[(title.x + 1, title.y)];
            assert!(cell.modifier.contains(Modifier::BOLD));
            assert_eq!(cell.bg, crate::tui::color_support::rgb(55, 55, 68));
            let selected = app.worktree_pane.selected_file.clone();
            let tree_path = app.worktree_pane.tree_selected_path.clone();
            let mode = app.files_inspector.mode();
            let offset = app.files_inspector.scroll_offset();
            let diff_offset = app.diff_pane_scroll;
            assert!(offset > 0);
            // Esc exits the Diff list or Files tree without changing the active tab.
            // Files Left intentionally traverses to Diff instead.
            match route {
                0 => app.handle_key(KeyCode::Esc, KeyModifiers::NONE).unwrap(),
                1 => {
                    let mut remote = crate::tui::backend::RemoteConnection::dummy();
                    rt.block_on(app.handle_remote_key(
                        KeyCode::Esc,
                        KeyModifiers::NONE,
                        &mut remote,
                    ))
                    .unwrap();
                }
                _ => super::remote::handle_disconnected_key(
                    &mut app,
                    KeyCode::Esc,
                    KeyModifiers::NONE,
                )
                .unwrap(),
            }
            assert!(!app.diff_pane_focus, "route={route} tab={tab:?}");
            render_and_snap(&app, &mut terminal);
            let cell = &terminal.backend().buffer()[(title.x + 1, title.y)];
            assert!(!cell.modifier.contains(Modifier::BOLD));
            assert_ne!(cell.bg, crate::tui::color_support::rgb(55, 55, 68));
            assert_eq!(app.worktree_pane.tab, tab);
            assert_eq!(app.worktree_pane.selected_file, selected);
            assert_eq!(app.worktree_pane.tree_selected_path, tree_path);
            assert_eq!(app.files_inspector.mode(), mode);
            assert_eq!(app.files_inspector.scroll_offset(), offset);
            assert_eq!(app.diff_pane_scroll, diff_offset);
            assert_eq!(worktree_rendered_region(&terminal, content_area), content);
        }
    }
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

fn route_worktree_arrow(app: &mut App, route: usize, code: KeyCode, rt: &tokio::runtime::Runtime) {
    match route {
        0 => app.handle_key(code, KeyModifiers::NONE).unwrap(),
        1 => {
            let mut remote = crate::tui::backend::RemoteConnection::dummy();
            rt.block_on(app.handle_remote_key(code, KeyModifiers::NONE, &mut remote))
                .unwrap();
        }
        _ => super::remote::handle_disconnected_key(app, code, KeyModifiers::NONE).unwrap(),
    }
}

#[test]
fn routed_worktree_arrows_retain_final_tab_content_after_focus_exit() {
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    use ratatui::style::Modifier;
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        for initial_tab in [Diff, Files] {
            let (_repo, mut app, mut terminal) = inspector_fixture();
            select_inspector_file(&mut app, &mut terminal, "script.py");
            assert!(app.files_inspector.select_next_mode(1));
            app.files_inspector.set_scroll_offset(7);
            app.set_worktree_pane_tab(initial_tab);
            app.worktree_pane.selected_file = Some("script.py".into());
            app.diff_pane_scroll = 3;
            app.diff_pane_auto_scroll = false;
            render_and_snap(&app, &mut terminal);
            if initial_tab == Files {
                // This is an intentional tab transition, not a pure focus exit.
                route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
                assert_eq!(app.worktree_pane.tab, Diff, "route={route}");
                assert!(app.diff_pane_focus, "Files -> Diff must not skip to Chat");
                render_and_snap(&app, &mut terminal);
                // Seed nondefault state on the FINAL tab. Tab switching may reset
                // selection/scroll, but the following focus exit must not do so.
                app.worktree_pane.selected_file = Some("script.py".into());
                app.diff_pane_scroll = 3;
                app.diff_pane_auto_scroll = false;
            }
            render_and_snap(&app, &mut terminal);
            let layout = crate::tui::ui::worktree_pane_layout().unwrap();
            let title = layout.diff_tab_area;
            let content = worktree_rendered_region(&terminal, layout.body_area);
            assert!(content.contains("function_"), "{content}");
            let state = (
                app.worktree_pane.selected_file.clone(),
                app.worktree_pane.tree_selected_path.clone(),
                app.files_inspector.mode(),
                app.files_inspector.scroll_offset(),
                app.diff_pane_scroll,
            );
            let cell = &terminal.backend().buffer()[(title.x + 1, title.y)];
            assert!(cell.modifier.contains(Modifier::BOLD));
            assert_eq!(cell.bg, crate::tui::color_support::rgb(55, 55, 68));
            route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
            assert!(
                !app.diff_pane_focus,
                "route={route} initial={initial_tab:?}"
            );
            assert_eq!(app.worktree_pane.tab, Diff);
            render_and_snap(&app, &mut terminal);
            assert_eq!(
                state,
                (
                    app.worktree_pane.selected_file.clone(),
                    app.worktree_pane.tree_selected_path.clone(),
                    app.files_inspector.mode(),
                    app.files_inspector.scroll_offset(),
                    app.diff_pane_scroll,
                )
            );
            assert_eq!(
                worktree_rendered_region(&terminal, layout.body_area),
                content
            );
            let cell = &terminal.backend().buffer()[(title.x + 1, title.y)];
            assert!(!cell.modifier.contains(Modifier::BOLD));
            assert_ne!(cell.bg, crate::tui::color_support::rgb(55, 55, 68));
            route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
            assert!(app.diff_pane_focus);
            assert_eq!(app.worktree_pane.tab, Diff);
            route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
            assert_eq!(app.worktree_pane.tab, Files);
            route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
            assert_eq!(app.worktree_pane.tab, Files);
        }
    }
}

#[test]
fn routed_worktree_tree_local_left_collapses_and_selects_parent() {
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        let (repo, mut app, mut terminal) = inspector_fixture();
        std::fs::create_dir(repo.path().join("nested")).unwrap();
        std::fs::write(repo.path().join("nested/child.rs"), "fn child() {}\n").unwrap();
        crate::tui::ui::prime_project_tree_for_tests(repo.path());
        // Existing contract: a nonempty composer disables the empty-composer
        // rail traversal. The focused Files tree then owns horizontal arrows.
        app.input = "draft".into();
        app.cursor_pos = app.input.len();
        render_and_snap(&app, &mut terminal);
        app.worktree_pane.tree_selected_path = Some("nested".into());
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        render_and_snap(&app, &mut terminal);
        assert!(
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .tree_rows
                .iter()
                .any(|row| row.path == "nested/child.rs")
        );
        app.worktree_pane.tree_selected_path = Some("nested/child.rs".into());
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(
            app.worktree_pane.tree_selected_path.as_deref(),
            Some("nested")
        );
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        render_and_snap(&app, &mut terminal);
        assert!(
            !crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .tree_rows
                .iter()
                .any(|row| row.path == "nested/child.rs")
        );
        assert!(app.diff_pane_focus);
        assert_eq!(
            app.worktree_pane.tab,
            super::worktree_pane::WorktreePaneTab::Files
        );
        assert_eq!(app.input, "draft");
        // With the same focused tree but an empty composer, the rail chain
        // takes precedence even when Left could collapse or select a parent.
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        render_and_snap(&app, &mut terminal);
        app.worktree_pane.tree_selected_path = Some("nested/child.rs".into());
        app.input.clear();
        app.cursor_pos = 0;
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(
            app.worktree_pane.tab,
            super::worktree_pane::WorktreePaneTab::Diff
        );
        assert!(app.diff_pane_focus);
        assert_eq!(
            app.worktree_pane.tree_selected_path.as_deref(),
            Some("nested/child.rs")
        );
        app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);
        app.worktree_pane.tree_selected_path = Some("nested".into());
        render_and_snap(&app, &mut terminal);
        assert!(
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .tree_rows
                .iter()
                .any(|row| row.path == "nested" && row.expanded)
        );
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(
            app.worktree_pane.tab,
            super::worktree_pane::WorktreePaneTab::Diff
        );
        assert!(app.diff_pane_focus);
    }
}

#[test]
fn routed_worktree_inspector_left_returns_to_tree_before_two_stop_rail_exit() {
    use super::worktree_pane::WorktreePaneTab::{Diff, Files};
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        let (_repo, mut app, mut terminal) = inspector_fixture();
        select_inspector_file(&mut app, &mut terminal, "script.py");
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        assert!(app.files_inspector.is_focused());
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        assert_eq!(
            app.files_inspector.mode(),
            Some(super::files_inspector::FileInspectorMode::Changes)
        );
        render_and_snap(&app, &mut terminal);
        route_worktree_arrow(&mut app, route, KeyCode::PageDown, &rt);
        render_and_snap(&app, &mut terminal);
        let offset = app.files_inspector.scroll_offset();
        assert!(offset > 0);
        let before = crate::tui::ui::worktree_pane_layout().unwrap();
        assert_eq!(before.preview_scroll, usize::from(offset));
        let preview = crate::tui::ui::worktree_pane_layout()
            .unwrap()
            .preview_area
            .unwrap();
        let content = worktree_rendered_region(&terminal, preview);
        assert!(content.contains("function_"));
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert!(!app.files_inspector.is_focused());
        assert!(app.diff_pane_focus);
        assert_eq!(app.worktree_pane.tab, Files);
        assert_eq!(app.files_inspector.scroll_offset(), offset);
        assert_eq!(
            app.worktree_pane.tree_selected_path.as_deref(),
            Some("script.py")
        );
        assert_eq!(
            app.files_inspector.mode(),
            Some(super::files_inspector::FileInspectorMode::Changes)
        );
        render_and_snap(&app, &mut terminal);
        let after = crate::tui::ui::worktree_pane_layout().unwrap();
        assert_eq!(after.preview_area, Some(preview));
        assert_eq!(after.preview_scroll, before.preview_scroll);
        assert_eq!(worktree_rendered_region(&terminal, preview), content);
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        render_and_snap(&app, &mut terminal);
        assert!(app.files_inspector.is_focused());
        assert_eq!(app.files_inspector.scroll_offset(), offset);
        assert_eq!(
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .preview_scroll,
            before.preview_scroll
        );
        assert_eq!(worktree_rendered_region(&terminal, preview), content);
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        render_and_snap(&app, &mut terminal);
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert!(app.diff_pane_focus);
        assert_eq!(app.worktree_pane.tab, Diff);
        render_and_snap(&app, &mut terminal);
        let body = crate::tui::ui::worktree_pane_layout().unwrap().body_area;
        let final_content = worktree_rendered_region(&terminal, body);
        let final_selection = app.worktree_pane.selected_file.clone();
        let final_scroll = app.diff_pane_scroll;
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert!(!app.diff_pane_focus);
        assert_eq!(app.worktree_pane.tab, Diff);
        render_and_snap(&app, &mut terminal);
        assert_eq!(app.worktree_pane.selected_file, final_selection);
        assert_eq!(app.diff_pane_scroll, final_scroll);
        assert_eq!(worktree_rendered_region(&terminal, body), final_content);
    }
}

// Keep the small semantic fixture unchanged. These documents deliberately exceed
// the preview height in Read, Source and Changes, with unique visible line labels.
fn long_inspector_fixture() -> (
    tempfile::TempDir,
    App,
    ratatui::Terminal<ratatui::backend::TestBackend>,
) {
    let (repo, app, terminal) = inspector_fixture();
    let markdown = |version: &str| {
        (0..100)
            .map(|line| format!("{version} paragraph_{line:03}\n\n"))
            .collect::<String>()
    };
    std::fs::write(repo.path().join("README.md"), markdown("original")).unwrap();
    for args in [
        vec!["add", "README.md"],
        vec!["commit", "-qm", "long markdown baseline"],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(repo.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::write(repo.path().join("README.md"), markdown("changed")).unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    (repo, app, terminal)
}

fn inspector_view(
    app: &App,
    terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>,
) -> (ratatui::layout::Rect, usize, String) {
    render_and_snap(app, terminal);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    let body = layout.preview_area.unwrap();
    (
        body,
        layout.preview_scroll,
        worktree_rendered_region(terminal, body),
    )
}

#[test]
fn routed_worktree_inspector_focus_return_preserves_all_long_modes() {
    use super::files_inspector::FileInspectorMode::{Changes, Read, Source};
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        let (_repo, mut app, mut terminal) = long_inspector_fixture();
        for (path, modes, distinctive) in [
            ("README.md", vec![Read, Source, Changes], "paragraph_"),
            ("script.py", vec![Source, Changes], "function_"),
        ] {
            select_inspector_file(&mut app, &mut terminal, path);
            route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
            for (index, mode) in modes.into_iter().enumerate() {
                if index > 0 {
                    route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
                }
                render_and_snap(&app, &mut terminal);
                assert_eq!(app.files_inspector.mode(), Some(mode));
                let top = inspector_view(&app, &mut terminal);
                route_worktree_arrow(&mut app, route, KeyCode::PageDown, &rt);
                let before = inspector_view(&app, &mut terminal);
                let offset = app.files_inspector.scroll_offset();
                let layout = crate::tui::ui::worktree_pane_layout().unwrap();
                assert!(layout.preview_total_lines > before.0.height as usize);
                assert!(offset > 0 && before.1 > 0);
                assert_ne!(before.2, top.2);
                assert!(
                    before.2.contains(distinctive),
                    "route={route} {path} {mode:?}: {}",
                    before.2
                );
                for exit in [KeyCode::Left, KeyCode::Char('h'), KeyCode::Esc] {
                    route_worktree_arrow(&mut app, route, exit, &rt);
                    assert!(!app.files_inspector.is_focused());
                    assert!(app.diff_pane_focus);
                    assert_eq!(
                        app.worktree_pane.tab,
                        super::worktree_pane::WorktreePaneTab::Files
                    );
                    assert_eq!(app.worktree_pane.tree_selected_path.as_deref(), Some(path));
                    assert_eq!(app.files_inspector.mode(), Some(mode));
                    assert_eq!(app.files_inspector.scroll_offset(), offset);
                    assert_eq!(
                        inspector_view(&app, &mut terminal),
                        before,
                        "route={route} {path} {mode:?} {exit:?}"
                    );
                    route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
                    assert!(app.files_inspector.is_focused());
                    assert_eq!(app.files_inspector.scroll_offset(), offset);
                    assert_eq!(inspector_view(&app, &mut terminal), before);
                }
            }
            route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
            render_and_snap(&app, &mut terminal);
        }
    }
}

#[test]
fn routed_worktree_inspector_resize_clamps_only_effective_offset() {
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        let (_repo, mut app, mut terminal) = long_inspector_fixture();
        select_inspector_file(&mut app, &mut terminal, "script.py");
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        render_and_snap(&app, &mut terminal);
        route_worktree_arrow(&mut app, route, KeyCode::End, &rt);
        let stored = app.files_inspector.scroll_offset();
        let mode = app.files_inspector.mode();
        assert!(stored > 0);
        for height in [40, 30] {
            terminal.backend_mut().resize(140, height);
            terminal
                .resize(ratatui::layout::Rect::new(0, 0, 140, height))
                .unwrap();
            let before = inspector_view(&app, &mut terminal);
            let layout = crate::tui::ui::worktree_pane_layout().unwrap();
            let max_scroll = layout.preview_total_lines - before.0.height as usize;
            assert_eq!(before.1, usize::from(stored).min(max_scroll));
            if height == 40 {
                assert!(
                    before.1 < usize::from(stored),
                    "larger body must exercise clamping"
                );
            }
            for key in [KeyCode::Left, KeyCode::Enter] {
                route_worktree_arrow(&mut app, route, key, &rt);
                assert_eq!(inspector_view(&app, &mut terminal), before);
                assert_eq!(app.files_inspector.scroll_offset(), stored);
                assert_eq!(app.files_inspector.mode(), mode);
            }
        }
        terminal.backend_mut().resize(70, 24);
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, 70, 24))
            .unwrap();
        render_and_snap(&app, &mut terminal);
        assert!(crate::tui::ui::worktree_pane_layout().is_none());
        route_worktree_arrow(&mut app, route, KeyCode::Down, &rt);
        assert!(!app.files_inspector.is_focused());
        terminal.backend_mut().resize(140, 30);
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, 140, 30))
            .unwrap();
        let restored = inspector_view(&app, &mut terminal);
        assert_eq!(restored.1, usize::from(stored));
        assert_eq!(app.files_inspector.scroll_offset(), stored);
        assert_eq!(app.files_inspector.mode(), mode);
        assert_eq!(
            app.worktree_pane.tree_selected_path.as_deref(),
            Some("script.py")
        );
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), restored);
    }
}

#[test]
fn files_inspector_scroll_is_per_file_and_mode() {
    use super::files_inspector::FileInspectorMode::{Changes, Read, Source};
    let _lock = scroll_render_test_lock();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    for route in 0..3 {
        let (_repo, mut app, mut terminal) = long_inspector_fixture();
        select_inspector_file(&mut app, &mut terminal, "script.py");
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        for _ in 0..3 {
            route_worktree_arrow(&mut app, route, KeyCode::Down, &rt);
            render_and_snap(&app, &mut terminal);
        }
        let source = inspector_view(&app, &mut terminal);
        assert_eq!(source.1, 3);
        assert!(source.2.contains("function_3"));
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        render_and_snap(&app, &mut terminal);
        for _ in 0..5 {
            route_worktree_arrow(&mut app, route, KeyCode::Down, &rt);
            render_and_snap(&app, &mut terminal);
        }
        let changes = inspector_view(&app, &mut terminal);
        assert_eq!(changes.1, 5);
        assert_ne!(source.2, changes.2);
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), changes);
        select_inspector_file(&mut app, &mut terminal, "README.md");
        assert_eq!(app.files_inspector.mode(), Some(Read));
        assert_eq!(app.files_inspector.scroll_offset(), 0);
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        render_and_snap(&app, &mut terminal);
        for _ in 0..7 {
            route_worktree_arrow(&mut app, route, KeyCode::Down, &rt);
            render_and_snap(&app, &mut terminal);
        }
        let markdown = inspector_view(&app, &mut terminal);
        assert_eq!(markdown.1, 7);
        assert!(markdown.2.contains("paragraph_"));
        assert_ne!(markdown.2, changes.2);
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), markdown);
        select_inspector_file(&mut app, &mut terminal, "script.py");
        assert_eq!(
            app.files_inspector.mode(),
            Some(Source),
            "file switch restores default mode, not last mode"
        );
        assert_eq!(app.files_inspector.scroll_offset(), 3);
        assert_eq!(inspector_view(&app, &mut terminal), source);
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), source);
        route_worktree_arrow(&mut app, route, KeyCode::Right, &rt);
        assert_eq!(app.files_inspector.mode(), Some(Changes));
        assert_eq!(app.files_inspector.scroll_offset(), 5);
        assert_eq!(inspector_view(&app, &mut terminal), changes);
        route_worktree_arrow(&mut app, route, KeyCode::Left, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), changes);
        select_inspector_file(&mut app, &mut terminal, "README.md");
        assert_eq!(app.files_inspector.mode(), Some(Read));
        assert_eq!(app.files_inspector.scroll_offset(), 7);
        assert_eq!(inspector_view(&app, &mut terminal), markdown);
        route_worktree_arrow(&mut app, route, KeyCode::Enter, &rt);
        assert_eq!(inspector_view(&app, &mut terminal), markdown);
    }
}

#[test]
fn files_inspector_tree_pointer_and_wheel_preserve_selection_contract() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = inspector_fixture();
    for file in 0..30 {
        let source = (0..100)
            .map(|line| format!("file_{file:02}_line_{line:03}\n"))
            .collect::<String>();
        std::fs::write(repo.path().join(format!("z{file:02}.txt")), source).unwrap();
    }
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    select_inspector_file(&mut app, &mut terminal, "script.py");
    // Actual tree keys scroll the list. No seeded selection/scroll state.
    for _ in 0..12 {
        app.handle_key(KeyCode::Down, KeyModifiers::NONE).unwrap();
        render_and_snap(&app, &mut terminal);
    }
    let selected = app.worktree_pane.tree_selected_path.clone().unwrap();
    app.handle_key(KeyCode::Enter, KeyModifiers::NONE).unwrap();
    render_and_snap(&app, &mut terminal);
    app.handle_key(KeyCode::PageDown, KeyModifiers::NONE)
        .unwrap();
    let before = inspector_view(&app, &mut terminal);
    assert!(before.1 > 0);
    let distinctive = format!("file_{}_line_", &selected[1..3]);
    assert!(before.2.contains(&distinctive));
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(inspector_view(&app, &mut terminal), before);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    assert!(layout.tree_scroll > 0);
    let index = layout
        .tree_rows
        .iter()
        .position(|row| row.path == selected)
        .unwrap();
    let tree = layout.tree_area.unwrap();
    for row_index in [index, index - 1] {
        let y = tree.y + (row_index - layout.tree_scroll) as u16;
        assert!(y < tree.bottom());
        app.handle_mouse_event(worktree_test_mouse(MouseEventKind::Moved, tree.x + 1, y));
        assert_eq!(inspector_view(&app, &mut terminal), before);
        assert_eq!(
            app.worktree_pane.tree_selected_path.as_deref(),
            Some(selected.as_str())
        );
        assert_eq!(app.worktree_pane.tree_scroll, layout.tree_scroll);
    }
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::ScrollDown,
        tree.x + 1,
        tree.y,
    ));
    let changed = inspector_view(&app, &mut terminal);
    let target = &layout.tree_rows[index + 3].path;
    assert_eq!(
        app.worktree_pane.tree_selected_path.as_deref(),
        Some(target.as_str())
    );
    assert!(app.worktree_pane.tree_scroll > layout.tree_scroll);
    assert!(!app.files_inspector.is_focused());
    assert_eq!(
        app.files_inspector.mode(),
        Some(super::files_inspector::FileInspectorMode::Source)
    );
    assert_eq!(changed.1, 0);
    assert!(
        changed
            .2
            .contains(&format!("file_{}_line_000", &target[1..3]))
    );
    assert_ne!(changed.2, before.2);
    let layout = crate::tui::ui::worktree_pane_layout().unwrap();
    let tree = layout.tree_area.unwrap();
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::ScrollUp,
        tree.x + 1,
        tree.y,
    ));
    assert_eq!(inspector_view(&app, &mut terminal), before);
    assert_eq!(
        app.worktree_pane.tree_selected_path.as_deref(),
        Some(selected.as_str())
    );
    assert_eq!(usize::from(app.files_inspector.scroll_offset()), before.1);
    assert!(!app.files_inspector.is_focused());
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
    let preview = layout.preview_area.unwrap();
    let before = worktree_rendered_region(&terminal, preview);
    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::ScrollDown,
        layout.preview_area.expect("preview area").x + 1,
        layout.preview_area.expect("preview area").y + 1,
    ));
    render_and_snap(&app, &mut terminal);
    let after = crate::tui::ui::worktree_pane_layout().unwrap();
    let content = worktree_rendered_region(&terminal, preview);
    assert_eq!(after.preview_area, Some(preview));
    assert_eq!(after.preview_scroll, 3);
    assert_ne!(content, before);
    assert!(content.contains("function_3"), "{content}");
    for key in [KeyCode::Enter, KeyCode::Left] {
        app.handle_key(key, KeyModifiers::NONE).unwrap();
        render_and_snap(&app, &mut terminal);
        assert_eq!(
            crate::tui::ui::worktree_pane_layout()
                .unwrap()
                .preview_scroll,
            3
        );
        assert_eq!(worktree_rendered_region(&terminal, preview), content);
    }
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
    let backend = ratatui::backend::TestBackend::new(140, 30);
    let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");

    select_inspector_file(&mut app, &mut terminal, "demo.rs");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.files_inspector.is_focused());
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
    let pane = crate::tui::ui::last_layout_snapshot()
        .expect("rendered layout")
        .diff_pane_area
        .expect("generic side-panel area");

    app.handle_mouse_event(worktree_test_mouse(
        MouseEventKind::ScrollDown,
        pane.x + 1,
        pane.y + 1,
    ));

    assert!(
        app.diff_pane_scroll > 0,
        "wheel over document body should advance the rendered side pane"
    );
    assert!(!app.diff_pane_focus, "wheel must not change keyboard focus");
}

#[test]
fn test_worktree_surface_has_only_diff_and_files_and_two_stop_arrow_cycle() {
    let _lock = scroll_render_test_lock();
    let mut app = create_test_app();
    app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Diff);
    app.set_diff_pane_focus(false);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
    render_and_snap(&app, &mut terminal);
    let text = terminal.backend().to_string();
    assert!(text.contains("Diff"));
    assert!(text.contains("Files"));
    assert!(!text.contains("Documents"));
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert!(app.diff_pane_focus);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, super::worktree_pane::WorktreePaneTab::Files);
    app.handle_key(KeyCode::Right, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, super::worktree_pane::WorktreePaneTab::Files);
    app.handle_key(KeyCode::Left, KeyModifiers::NONE).unwrap();
    assert_eq!(app.worktree_pane.tab, super::worktree_pane::WorktreePaneTab::Diff);
}

#[test]
fn test_generic_side_panel_pages_remain_visible_without_documents_tab() {
    let _lock = scroll_render_test_lock();
    for source in [
        crate::side_panel::SidePanelPageSource::Managed,
        crate::side_panel::SidePanelPageSource::Ephemeral,
        crate::side_panel::SidePanelPageSource::LinkedFile,
    ] {
        let mut app = create_test_app();
        app.set_worktree_pane_tab(super::worktree_pane::WorktreePaneTab::Files);
        app.set_diff_pane_focus(true);
        let mut snapshot = document_snapshot("generic", &[("generic", "Generic", "SIDE_UNIQUE")]);
        snapshot.pages[0].source = source;
        app.apply_side_panel_snapshot(snapshot);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();
        render_and_snap(&app, &mut terminal);
        assert!(terminal.backend().to_string().contains("SIDE_UNIQUE"));
        assert_eq!(app.worktree_pane.tab, super::worktree_pane::WorktreePaneTab::Files);
        assert!(!app.worktree_pane_explicit_open());
        app.handle_key(KeyCode::Char('m'), KeyModifiers::ALT).unwrap();
        render_and_snap(&app, &mut terminal);
        assert!(!terminal.backend().to_string().contains("SIDE_UNIQUE"));
        app.handle_key(KeyCode::Char('m'), KeyModifiers::ALT).unwrap();
        render_and_snap(&app, &mut terminal);
        assert!(terminal.backend().to_string().contains("SIDE_UNIQUE"));
    }
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
        app.side_panel.pages.is_empty(),
        "Files Markdown activation must not create a generic side-panel page"
    );
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
fn test_files_tree_non_markdown_activation_keeps_inline_preview_focus() {
    let _lock = scroll_render_test_lock();
    let repo = init_worktree_pane_test_repo();
    crate::tui::ui::prime_project_tree_for_tests(repo.path());
    let mut app = create_test_app();
    app.session.working_dir = Some(repo.path().to_string_lossy().into_owned());
    app.open_project_files_pane();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 30)).unwrap();

    select_inspector_file(&mut app, &mut terminal, "demo.rs");
    assert!(app.handle_diff_pane_focus_key(KeyCode::Right, KeyModifiers::NONE));
    assert!(app.files_inspector.is_focused());
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
    assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
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
        text.contains("fn new() {}") && !text.contains("second"),
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
fn test_worktree_filter_removed_file_and_session_switch_selects_replacement_without_session_leak() {
    let _lock = scroll_render_test_lock();
    let (repo, mut app, mut terminal) = worktree_filter_fixture();
    std::fs::write(repo.path().join("demo.rs"), "fn old() {}\n").unwrap();
    crate::tui::ui::prime_worktree_changes_for_tests(repo.path());
    assert!(app.update_worktree_file_filter(Instant::now()));
    assert_eq!(app.current_worktree_selected_file(), Some("notes.txt"));
    let text = render_and_snap(&app, &mut terminal);
    assert!(text.contains("second"));
    // Refresh already selected the remaining file. Clicking it again would
    // deliberately toggle the mouse filter off rather than establish it.
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
    app.apply_side_panel_snapshot(crate::side_panel::SidePanelSnapshot {
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
    });
    assert!(!app.worktree_pane.explicit_open);
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
fn test_worktree_filter_hidden_idle_expiry_restores_first_file_at_top() {
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
        assert_eq!(app.current_worktree_selected_file(), Some("demo.rs"));
        app.help_scroll = None;
        let text = render_and_snap(&app, &mut wide);
        assert!(
            text.contains("fn new() {}") && !text.contains("second"),
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
    let (_repo, mut app, mut terminal) = diff_route_fixture();
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
    let (_repo, mut app, mut terminal) = diff_route_fixture();
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
